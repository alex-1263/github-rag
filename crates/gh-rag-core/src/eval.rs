//! LLM 裁判检索评测(RAGAS context_precision 思路 + BeIR 指标)。
//!
//! 流程:`query_log` 近 N 天去重真实查询 → 每题 hybrid top-k → LLM 逐条 0/1/2 相关性 →
//! 四指标(ndcg@5 / mrr / hit@5 / recall@5)+ 锚定集漂移防护。
//!
//! IO 边界:`Judge` trait 隔离 HTTP;测试用 `FixedJudge` 替身,零网络。

use crate::embedder::Embedder;
use crate::retrieve::{hybrid_search_with_query, SearchFilter, SearchParams};
use crate::store::IssueStore;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 裁判提示词(固定中文指令,温度 0;prompt_hash 源)。
pub const JUDGE_PROMPT: &str = "你是检索质量裁判。给定用户查询与一条候选文档(标题+摘要),判断该文档对查询的相关性并打分:2=直接回答或精确命中查询意图,1=部分相关,0=无关。只输出 JSON:{\"grade\": <0|1|2>, \"reason\": \"一句话中文理由\"}";

/// prompt 指纹(sha1 十六进制),用于追溯裁判口径漂移。
pub fn prompt_hash() -> String {
    use sha1::{Digest, Sha1};
    let mut h = Sha1::new();
    h.update(JUDGE_PROMPT.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// 指标纯函数(BeIR 同式;输入为按排名序的分级相关性,只取前 5)
// ---------------------------------------------------------------------------

/// NDCG@5,分级增益 2^rel - 1。
pub fn ndcg_at5(ranked: &[u8]) -> f64 {
    let dcg: f64 = ranked
        .iter()
        .take(5)
        .enumerate()
        .map(|(i, &r)| (2f64.powi(r as i32) - 1.0) / (i as f64 + 2.0).log2())
        .sum();
    let mut ideal: Vec<u8> = ranked.to_vec();
    ideal.sort_unstable_by(|a, b| b.cmp(a));
    let idcg: f64 = ideal
        .iter()
        .take(5)
        .enumerate()
        .map(|(i, &r)| (2f64.powi(r as i32) - 1.0) / (i as f64 + 2.0).log2())
        .sum();
    if idcg == 0.0 {
        0.0
    } else {
        dcg / idcg
    }
}

/// MRR:首个相关(>0)位次的倒数;无相关 = 0。
pub fn mrr(ranked: &[u8]) -> f64 {
    ranked
        .iter()
        .take(5)
        .position(|&r| r > 0)
        .map(|i| 1.0 / (i as f64 + 1.0))
        .unwrap_or(0.0)
}

/// Hit@5:前 5 含任一相关(>0)= 1。
pub fn hit_at5(ranked: &[u8]) -> f64 {
    if ranked.iter().take(5).any(|&r| r > 0) {
        1.0
    } else {
        0.0
    }
}

/// Recall@5 = 前五内相关数 / 全体相关数(total_relevant = 0 时定义为 0)。
pub fn recall_at5(ranked: &[u8], total_relevant: usize) -> f64 {
    if total_relevant == 0 {
        return 0.0;
    }
    let found = ranked.iter().take(5).filter(|&&r| r > 0).count();
    found as f64 / total_relevant as f64
}

// ---------------------------------------------------------------------------
// Judge trait 与实现
// ---------------------------------------------------------------------------

/// LLM 裁判抽象:对 (查询, 标题, 摘要) 打 0/1/2 分。HTTP 实现不进测试。
pub trait Judge {
    fn grade(&self, query: &str, doc_title: &str, doc_snippet: &str) -> Result<u8>;

    /// 带理由打分;默认降级为只返回分数。
    fn grade_reason(
        &self,
        query: &str,
        doc_title: &str,
        doc_snippet: &str,
    ) -> Result<(u8, Option<String>)> {
        Ok((self.grade(query, doc_title, doc_snippet)?, None))
    }
}

/// 测试替身:按标题查表给分,未命中则报错(可设 always 兜底)。
pub struct FixedJudge {
    by_title: HashMap<String, u8>,
    always: Option<u8>,
}

impl FixedJudge {
    pub fn by_title(map: HashMap<String, u8>) -> Self {
        Self {
            by_title: map,
            always: None,
        }
    }

    pub fn always(grade: u8) -> Self {
        Self {
            by_title: HashMap::new(),
            always: Some(grade),
        }
    }
}

impl Judge for FixedJudge {
    fn grade(&self, _query: &str, doc_title: &str, _doc_snippet: &str) -> Result<u8> {
        self.by_title
            .get(doc_title)
            .or(self.always.as_ref())
            .copied()
            .ok_or_else(|| Error::Eval(format!("FixedJudge 未配置标题 `{doc_title}` 的分值")))
    }
}

/// OpenAI 兼容 /chat/completions 裁判(ureq;重试 1 次;不进测试)。
pub struct HttpJudge {
    endpoint: String,
    api_key: String,
    model: String,
    agent: ureq::Agent,
}

impl HttpJudge {
    pub fn new(base_url: &str, api_key: &str, model: &str) -> Self {
        Self {
            endpoint: format!("{}/chat/completions", base_url.trim_end_matches('/')),
            api_key: api_key.to_string(),
            model: model.to_string(),
            agent: ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(60))
                .build(),
        }
    }
}

impl Judge for HttpJudge {
    fn grade_reason(
        &self,
        query: &str,
        doc_title: &str,
        doc_snippet: &str,
    ) -> Result<(u8, Option<String>)> {
        let user = format!("查询:{query}\n标题:{doc_title}\n摘要:{doc_snippet}");
        let body = serde_json::json!({
            "model": self.model,
            "temperature": 0,
            "response_format": {"type": "json_object"},
            "messages": [
                {"role": "system", "content": JUDGE_PROMPT},
                {"role": "user", "content": user},
            ],
        });
        let mut last: Option<Error> = None;
        for _ in 0..2 {
            match self
                .agent
                .post(&self.endpoint)
                .set("Authorization", &format!("Bearer {}", self.api_key))
                .send_json(body.clone())
            {
                Ok(resp) => {
                    let v: serde_json::Value = resp
                        .into_json()
                        .map_err(|e| Error::Eval(format!("裁判响应非 JSON: {e}")))?;
                    let content =
                        v["choices"][0]["message"]["content"]
                            .as_str()
                            .ok_or_else(|| {
                                Error::Eval("裁判响应缺 choices[0].message.content".into())
                            })?;
                    let parsed: serde_json::Value = serde_json::from_str(content.trim())
                        .map_err(|e| Error::Eval(format!("裁判输出非 JSON: {e}: {content}")))?;
                    let grade = parsed["grade"].as_u64().unwrap_or(99);
                    if grade > 2 {
                        return Err(Error::Eval(format!("裁判 grade 越界: {grade}")));
                    }
                    return Ok((
                        grade as u8,
                        parsed["reason"].as_str().map(|s| s.to_string()),
                    ));
                }
                Err(e) => last = Some(Error::Eval(format!("裁判请求失败: {e}"))),
            }
        }
        Err(last.unwrap_or_else(|| Error::Eval("裁判请求失败".into())))
    }

    fn grade(&self, query: &str, doc_title: &str, doc_snippet: &str) -> Result<u8> {
        self.grade_reason(query, doc_title, doc_snippet)
            .map(|(g, _)| g)
    }
}

// ---------------------------------------------------------------------------
// 评测报告与主流程
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct Anchor {
    pub query: String,
    pub repo: String,
    pub number: i64,
    pub expected_grade: u8,
}

#[derive(Debug, Clone, Serialize)]
pub struct HitEval {
    pub repo: String,
    pub number: i64,
    pub title: String,
    pub grade: u8,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueryEval {
    pub query: String,
    pub hits: Vec<HitEval>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Scores {
    pub ndcg_at5: f64,
    pub mrr: f64,
    pub hit_at5: f64,
    /// 池内召回代理:前五相关数 / 全部已判相关数(top_k=5 时恒为 1,扩池才有意义)
    pub recall_at5: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvalReport {
    pub scores: Scores,
    pub judge_model: String,
    pub prompt_hash: String,
    /// 锚定漂移防护:不一致比例 > 20% 时置 Some(原因)
    pub invalid: Option<String>,
    pub per_query: Vec<QueryEval>,
    /// 随机抽 5 条判例(人工 30 秒扫描用)
    pub samples: Vec<HitEval>,
}

#[derive(Debug, Clone, Copy)]
pub struct EvalParams {
    pub days: u32,
    pub top_k: usize,
}

impl Default for EvalParams {
    fn default() -> Self {
        Self { days: 7, top_k: 5 }
    }
}

/// 近 N 天去重真实查询(仅 tool = search_issues)。
pub fn recent_queries(store: &IssueStore, days: u32) -> Result<Vec<String>> {
    let sql = format!(
        "SELECT DISTINCT query FROM query_log \
         WHERE tool = 'search_issues' AND ts >= datetime('now', '-{days} days') \
         ORDER BY query"
    );
    let mut stmt = store.db.prepare(&sql)?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn anchor_load_queries(anchors: &[Anchor], out: &mut Vec<String>) {
    for a in anchors {
        if !out.iter().any(|q| q == &a.query) {
            out.push(a.query.clone());
        }
    }
}

/// 全量评测:见模块注释。anchors 混入必判并做漂移防护。
pub fn run_eval(
    store: &IssueStore,
    embedder: &dyn Embedder,
    judge: &dyn Judge,
    params: EvalParams,
    anchors: &[Anchor],
) -> Result<EvalReport> {
    let mut queries = recent_queries(store, params.days)?;
    anchor_load_queries(anchors, &mut queries);
    if queries.is_empty() {
        return Err(Error::Eval(
            "query_log 近期内无查询,先跑 MCP 检索积累日志".into(),
        ));
    }

    // 锚点索引:(query, repo, number) → expected_grade
    let mut anchor_idx: HashMap<(String, String, i64), u8> = HashMap::new();
    for a in anchors {
        anchor_idx.insert(
            (a.query.clone(), a.repo.clone(), a.number),
            a.expected_grade,
        );
    }
    let queries_with_anchors: usize = queries
        .iter()
        .filter(|&q| anchors.iter().any(|a| a.query == *q))
        .count();

    let mut per_query = Vec::new();
    let mut sum = Scores {
        ndcg_at5: 0.0,
        mrr: 0.0,
        hit_at5: 0.0,
        recall_at5: 0.0,
    };
    let mut recall_n = 0usize;
    let mut checked = 0usize;
    let mut mismatch = 0usize;
    let mut mismatch_samples: Vec<String> = Vec::new();

    for q in &queries {
        let qv = embedder.embed_query(q)?;
        let hits = hybrid_search_with_query(
            store,
            &qv,
            q,
            &SearchFilter::default(),
            params.top_k,
            &SearchParams::default(),
        )?;
        let mut hit_evals = Vec::with_capacity(hits.len());
        for h in &hits {
            let (grade, reason) = judge.grade_reason(q, &h.title, &h.snippet)?;
            hit_evals.push(HitEval {
                repo: h.repo.clone(),
                number: h.number,
                title: h.title.clone(),
                grade,
                reason,
            });
            if let Some(&expected) = anchor_idx.get(&(q.clone(), h.repo.clone(), h.number)) {
                checked += 1;
                if grade != expected {
                    mismatch += 1;
                    if mismatch_samples.len() < 3 {
                        mismatch_samples.push(format!(
                            "{q} → {}/#{}: 期望 {expected},裁判 {grade}",
                            h.repo, h.number
                        ));
                    }
                }
            }
        }
        let grades: Vec<u8> = hit_evals.iter().map(|h| h.grade).collect();
        sum.ndcg_at5 += ndcg_at5(&grades);
        sum.mrr += mrr(&grades);
        sum.hit_at5 += hit_at5(&grades);
        let rel_total = grades.iter().filter(|&&g| g > 0).count();
        if rel_total > 0 {
            sum.recall_at5 += recall_at5(&grades, rel_total);
            recall_n += 1;
        }
        per_query.push(QueryEval {
            query: q.clone(),
            hits: hit_evals,
        });
    }

    let n = per_query.len() as f64;
    let scores = Scores {
        ndcg_at5: sum.ndcg_at5 / n,
        mrr: sum.mrr / n,
        hit_at5: sum.hit_at5 / n,
        recall_at5: if recall_n > 0 {
            sum.recall_at5 / recall_n as f64
        } else {
            0.0
        },
    };

    // 锚定漂移防护:不一致比例 > 20% → invalid
    let invalid = if checked > 0 && queries_with_anchors > 0 {
        let ratio = mismatch as f64 / checked as f64;
        if ratio > 0.2 {
            Some(format!(
                "锚定校准失效:不一致 {mismatch}/{checked}({:.0}%)> 20%,裁判口径漂移。样本:{}",
                ratio * 100.0,
                mismatch_samples.join(";")
            ))
        } else {
            None
        }
    } else {
        None
    };

    Ok(EvalReport {
        scores,
        judge_model: String::new(),
        prompt_hash: prompt_hash(),
        invalid,
        samples: sample_hits(&per_query, 5),
        per_query,
    })
}

/// 等距抽 `k` 条判例(确定性伪随机:跨全表取 stride)。
fn sample_hits(per_query: &[QueryEval], k: usize) -> Vec<HitEval> {
    let all: Vec<&HitEval> = per_query.iter().flat_map(|q| q.hits.iter()).collect();
    if all.is_empty() {
        return Vec::new();
    }
    let stride = (all.len() as f64 / k.min(all.len()) as f64)
        .floor()
        .max(1.0) as usize;
    (0..)
        .map(|i| i * stride)
        .take_while(|&i| i < all.len())
        .take(k)
        .map(|i| all[i].clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // 手算例:discounts = 1/log2(2), 1/log2(3), 1/log2(4)…
    // [2,2,2,0,0]:dcg = 2·1 + 2/1.58496 + 2/2 = 4.26186;理想同序 → ndcg = 1
    #[test]
    fn ndcg_perfect_ranking_is_one() {
        assert!((ndcg_at5(&[2, 2, 2, 0, 0]) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ndcg_all_irrelevant_is_zero() {
        assert_eq!(ndcg_at5(&[0, 0, 0, 0, 0]), 0.0);
        assert_eq!(ndcg_at5(&[]), 0.0);
    }

    // [0,1,0,0,2]:dcg = 1/log2(3) + 3/log2(6) = 0.63093 + 1.16056 = 1.79149
    // 理想序 [2,1,0,0,0]:idcg = 3 + 0.63093 = 3.63093 → ndcg = 0.49333
    #[test]
    fn ndcg_partial_hand_computed() {
        assert!((ndcg_at5(&[0, 1, 0, 0, 2]) - 1.791_488_2 / 3.630_929_8).abs() < 1e-6);
    }

    #[test]
    fn mrr_positions() {
        assert!((mrr(&[2, 2, 2, 0, 0]) - 1.0).abs() < 1e-9);
        assert!((mrr(&[0, 1, 0, 0, 2]) - 0.5).abs() < 1e-9);
        assert_eq!(mrr(&[0, 0, 0, 0, 0]), 0.0);
    }

    #[test]
    fn hit_at5_binary() {
        assert_eq!(hit_at5(&[0, 0, 1, 0, 0]), 1.0);
        assert_eq!(hit_at5(&[0, 0, 0, 0, 0]), 0.0);
    }

    #[test]
    fn recall_at5_counts_fraction() {
        // 前 5 内 2 条相关,共 3 条相关 → 2/3
        assert!((recall_at5(&[2, 0, 1, 0, 0, 2], 3) - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(recall_at5(&[2, 2, 2, 0, 0], 3), 1.0);
        assert_eq!(recall_at5(&[2, 2, 2, 0, 0], 0), 0.0);
    }

    #[test]
    fn prompt_hash_is_stable_sha1_hex() {
        assert_eq!(prompt_hash().len(), 40);
        assert!(prompt_hash().chars().all(|c| c.is_ascii_hexdigit()));
    }
}
