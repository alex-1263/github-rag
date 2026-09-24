//! 混合检索:向量召回 ∥ FTS5 召回 → RRF 融合。

use crate::{embedder::Embedder, store::IssueStore, Error, Result};

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub repo: String,
    pub number: i64,
    /// issue | pr
    pub kind: String,
    pub title: String,
    pub state: String,
    pub snippet: String,
    pub score: f32,
    /// vec / fts / vec+fts(双路命中)
    pub source: &'static str,
}

#[derive(Debug, Clone)]
pub struct SearchParams {
    pub vec_top: usize,
    pub fts_top: usize,
    pub rrf_k: usize,
    pub snippet_chars: usize,
}

impl Default for SearchParams {
    fn default() -> Self {
        Self {
            vec_top: 30,
            fts_top: 30,
            rrf_k: 60,
            snippet_chars: 200,
        }
    }
}

#[derive(Default)]
pub struct SearchFilter {
    pub repos: Option<Vec<String>>,
    pub state: Option<String>,
    pub labels: Option<Vec<String>>,
}

/// RRF 融合(纯函数)。
pub fn rrf_fuse(vec_rank: &[(i64, usize)], fts_rank: &[(i64, usize)], k: usize) -> Vec<(i64, f32)> {
    use std::collections::HashMap;
    let mut scores: HashMap<i64, f32> = HashMap::new();
    for (id, rank) in vec_rank.iter().chain(fts_rank.iter()) {
        *scores.entry(*id).or_insert(0.0) += 1.0 / (k + rank) as f32;
    }
    let mut out: Vec<(i64, f32)> = scores.into_iter().collect();
    out.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    out
}

/// 点积(两侧均须同维;长度不等 = 索引与查询向量空间不一致 → Err,不静默截断)。
fn dot(a: &[f32], b: &[f32]) -> Result<f32> {
    if a.len() != b.len() {
        return Err(Error::DimensionMismatch {
            expected: a.len(),
            got: b.len(),
        });
    }
    Ok(a.iter().zip(b.iter()).map(|(x, y)| x * y).sum())
}

pub fn hybrid_search(
    store: &IssueStore,
    embedder: &dyn Embedder,
    query: &str,
    filter: &SearchFilter,
    top_k: usize,
    params: &SearchParams,
) -> Result<Vec<SearchHit>> {
    let q = embedder.embed_query(query)?;
    let hits = hybrid_search_with_query(store, &q, query, filter, top_k, params, "search_issues")?;
    Ok(hits)
}

/// 过滤条件的 JSON 形状(log_query.filters 列)。
fn filters_json(f: &SearchFilter) -> serde_json::Value {
    let map = |v: &Option<Vec<String>>| -> serde_json::Value {
        match v {
            Some(list) => serde_json::json!(list),
            None => serde_json::Value::Null,
        }
    };
    serde_json::json!({
        "repos": map(&f.repos),
        "state": f.state,
        "labels": map(&f.labels),
    })
}

/// 已有查询向量(如 MCP 侧嵌入先行)的检索路径:不持库锁完成网络调用后再进库。
/// `tool` 为 query_log 记录的工具名(既有检索传 `search_issues`,CLI 传 `cli-search`,
/// 查重传 `check_duplicate`)——eval 取题只认 `search_issues`,别路查询勿混入)。
pub fn hybrid_search_with_query(
    store: &IssueStore,
    q: &[f32],
    query: &str,
    filter: &SearchFilter,
    top_k: usize,
    params: &SearchParams,
    tool: &str,
) -> Result<Vec<SearchHit>> {
    let repos = filter.repos.as_deref();
    let state = filter.state.as_deref();
    let labels = filter.labels.as_deref();

    // ① 向量召回:暴力点积(向量已 L2 归一化,点积 = 余弦)
    let mut vec_rank: Vec<(i64, usize)> = Vec::new();
    let candidates = store.candidates(repos, state, labels)?;
    if !candidates.is_empty() {
        let mut sims: Vec<(i64, f32)> = candidates
            .iter()
            .map(|(id, blob)| {
                let v = bytes_to_f32(blob);
                let s = dot(q, &v)?;
                Ok((*id, s))
            })
            .collect::<Result<Vec<_>>>()?;
        sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        vec_rank = sims
            .iter()
            .take(params.vec_top)
            .enumerate()
            .map(|(r, (id, _))| (*id, r + 1))
            .collect();
    }

    // ② BM25 召回
    let fts_rows = store.fts_search(query, repos, state, labels, params.fts_top)?;
    let fts_rank: Vec<(i64, usize)> = fts_rows
        .iter()
        .enumerate()
        .map(|(r, (id, _))| (*id, r + 1))
        .collect();

    // ③ RRF 融合 → meta → 输出
    let fused = rrf_fuse(&vec_rank, &fts_rank, params.rrf_k);
    let top_ids: Vec<i64> = fused.iter().take(top_k).map(|(id, _)| *id).collect();
    let metas = store.meta(&top_ids)?;
    let mut hits = Vec::with_capacity(metas.len());
    for (id, score) in fused.iter().take(top_k) {
        let Some(m) = metas.iter().find(|m| m.id == *id) else {
            continue;
        };
        let in_vec = vec_rank.iter().any(|(i, _)| i == id);
        let in_fts = fts_rank.iter().any(|(i, _)| i == id);
        hits.push(SearchHit {
            repo: m.repo.clone(),
            number: m.number,
            kind: m.kind.clone(),
            title: m.title.clone(),
            state: m.state.clone(),
            snippet: snippet(&m.body, params.snippet_chars),
            score: *score,
            source: if in_vec && in_fts {
                "vec+fts"
            } else if in_vec {
                "vec"
            } else {
                "fts"
            },
        });
    }

    // 查询日志:落在此处(MCP 走 with_query 路径),飞轮不漏记;失败不阻断检索
    let log_entries: Vec<(String, i64)> = hits.iter().map(|h| (h.repo.clone(), h.number)).collect();
    let _ = store.log_query(tool, query, Some(filters_json(filter)), &log_entries);

    Ok(hits)
}
/// (issue_id, repo, number, title, score)
pub type RelatedHit = (i64, String, i64, String, f32);

/// 与指定 issue 最相似的 N 条(纯向量,排除自身)。
pub fn find_related(
    store: &IssueStore,
    repo: &str,
    number: i64,
    top_k: usize,
    state: Option<&str>,
) -> Result<Vec<RelatedHit>> {
    let Some(m) = store.get_issue(repo, number)? else {
        return Ok(Vec::new());
    };
    let Some(blob) = store.get_embedding_blob(m.id)? else {
        return Ok(Vec::new());
    };
    let q = bytes_to_f32(&blob);
    let candidates = store.candidates(None, state, None)?;
    let mut sims: Vec<(i64, f32)> = candidates
        .iter()
        .filter_map(|(id, b)| {
            if *id == m.id {
                return None;
            }
            let v = bytes_to_f32(b);
            match dot(&q, &v) {
                Ok(s) => Some(Ok((*id, s))),
                Err(e) => Some(Err(e)),
            }
        })
        .collect::<Result<Vec<_>>>()?;
    sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let top: Vec<i64> = sims.iter().take(top_k).map(|(id, _)| *id).collect();
    let metas = store.meta(&top)?;
    Ok(top
        .iter()
        .filter_map(|id| {
            let s = sims.iter().find(|(i, _)| i == id).map(|(_, s)| *s)?;
            let mm = metas.iter().find(|x| x.id == *id)?;
            Some((mm.id, mm.repo.clone(), mm.number, mm.title.clone(), s))
        })
        .collect())
}

fn bytes_to_f32(b: &[u8]) -> Vec<f32> {
    (0..b.len() / 4)
        .map(|i| f32::from_le_bytes([b[i * 4], b[i * 4 + 1], b[i * 4 + 2], b[i * 4 + 3]]))
        .collect()
}

fn snippet(body: &str, max_chars: usize) -> String {
    body.trim().chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_legs_hit_ranks_first() {
        let vec = vec![(1_i64, 1_usize), (2, 2), (3, 3)];
        let fts = vec![(2_i64, 1_usize), (4, 2)];
        let out = rrf_fuse(&vec, &fts, 60);
        assert_eq!(out[0].0, 2, "id=2 在两路都靠前,必须排第一");
    }

    #[test]
    fn fusable_when_single_leg() {
        let out = rrf_fuse(&[(7, 1)], &[], 60);
        assert_eq!(out.len(), 1);
        assert!((out[0].1 - 1.0 / 61.0).abs() < 1e-6);
    }

    #[test]
    fn rrf_fuse_ties_break_by_id_ascending() {
        // 5 与 9 各只中一路同排名 → 分数相同;次键 id 升序保证确定性
        let out = rrf_fuse(&[(9_i64, 1_usize)], &[(5_i64, 1_usize)], 60);
        assert_eq!(out.len(), 2);
        assert!((out[0].1 - out[1].1).abs() < 1e-9, "分数应相同");
        assert_eq!(out[0].0, 5, "平分时 id 小者在前");
        assert_eq!(out[1].0, 9);
    }

    #[test]
    fn dot_dimension_mismatch_is_error() {
        assert!(dot(&[1.0, 2.0], &[1.0, 2.0, 3.0]).is_err());
        assert!((dot(&[1.0, 2.0], &[3.0, 4.0]).unwrap() - 11.0).abs() < 1e-6);
    }

    #[test]
    fn hybrid_search_with_query_dimension_mismatch_is_error() {
        let dir = std::env::temp_dir().join(format!("gh-rag-dim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = IssueStore::create_fixture(&dir.join("t.sqlite")).unwrap();
        store
            .db
            .execute(
                "INSERT INTO issues(id, repo, number, title, body, state) \
                 VALUES (1,'t/r',1,'x','x','open')",
                [],
            )
            .unwrap();
        // 索引向量 4 维,查询向量 8 维:必须 Err,不得静默 zip 截断
        store
            .db
            .execute(
                "INSERT INTO issues_vec(issue_id, embedding) VALUES (1, x'000000000000803f')",
                [],
            )
            .unwrap();
        let q = vec![0.5f32; 8];
        let err = hybrid_search_with_query(
            &store,
            &q,
            "x",
            &SearchFilter::default(),
            5,
            &SearchParams::default(),
            "search_issues",
        )
        .unwrap_err();
        assert!(
            matches!(err, crate::Error::DimensionMismatch { .. }),
            "实际:{err}"
        );
    }
}
