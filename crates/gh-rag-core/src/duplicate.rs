//! 查重:agent 起草新 issue 前检查是否已有重复(Tier 1-A,PROPOSAL 第一痛点)。
//!
//! 路径:草稿(标题+正文)按**索引侧同一文本组装**嵌入(指纹纪律:查询向量必须与
//! 索引向量同组装参数,否则空间不可比)→ `hybrid_search_with_query`(词面腿用草稿
//! 标题,向量腿承载完整草稿)→ 标题相似度加权重排 → 返回带 `title_sim` 的命中。
//!
//! query_log 落 `tool='check_duplicate'`:eval 取题只认 `search_issues`,查重流量
//! 不混入题库(P0 契约)。

use crate::cjk::cjk_bigram;
use crate::embedder::build_text_with_comments;
use crate::retrieve::{hybrid_search_with_query, SearchFilter, SearchHit, SearchParams};
use crate::store::IssueStore;
use crate::sync::{COMMENT_QUOTA, PER_COMMENT_MAX};
use crate::Result;

/// 查重命中。`score` 语义与检索一致(RRF 融合分,不因加权改写);`title_sim` 为
/// 草稿标题 vs 命中标题的 bigram 相似度,单独返回,由调用方(agent)自行判断。
#[derive(Debug, Clone)]
pub struct DuplicateHit {
    pub repo: String,
    pub number: i64,
    /// issue | pr
    pub kind: String,
    pub title: String,
    pub state: String,
    /// 检索分(RRF 融合分,语义不变)
    pub score: f32,
    /// 草稿标题与命中标题的相似度 [0,1]
    pub title_sim: f32,
    /// vec / fts / vec+fts(双路命中)
    pub source: &'static str,
}

/// 草稿查重的嵌入文本:复用索引侧同一组装点 `build_text_with_comments`,草稿无评论
/// → 空评论表退化为基础形态(标题重复加权 + 图片降噪 + 正文截断)。
/// 不许另造组装式:指纹纪律含文本组装参数,另造 = 查询向量与索引空间错位。
pub fn draft_text(title: &str, body: &str, title_repeats: usize, body_max_chars: usize) -> String {
    build_text_with_comments(
        title,
        body,
        &[],
        title_repeats,
        body_max_chars,
        COMMENT_QUOTA,
        PER_COMMENT_MAX,
    )
}

/// 已有草稿向量的查重。调用方先用 [`draft_text`] 组装并嵌入(组装参数与索引侧
/// 同源)——MCP 嵌入先行不持库锁,测试同形组合,两条路径同一入口。
///
/// 词面(FTS5)腿查询用**草稿标题**:fts_search 是 token 隐式 AND,喂整份草稿正文
/// (几百 token)必然空手而归——词面腿就此死亡,重复检测只剩向量单腿。标题是重复
/// 报告的最高精度信号(同一故障,标题措辞近似),短且稳,AND 语义恰好表达
/// 「词面全部命中」。向量腿已承载完整草稿语义,分工不重叠。
pub fn check_duplicate_with_query(
    store: &IssueStore,
    q: &[f32],
    draft_title: &str,
    filter: &SearchFilter,
    top_k: usize,
    params: &SearchParams,
) -> Result<Vec<DuplicateHit>> {
    // 召回池放宽到 3×top_k(下限 15):检索分排后的命中可能因标题近似被提进 top_k,
    // 必须先取宽池、组合重排后再截断;先截断后加权会漏掉「正文写得不同」的真重复。
    // (池上限天然受 vec_top/fts_top 约束,不会放大扫描量。)
    let pool = top_k.saturating_mul(3).max(15);
    let hits = hybrid_search_with_query(
        store,
        q,
        draft_title,
        filter,
        pool,
        params,
        "check_duplicate",
    )?;
    Ok(rerank(hits, draft_title, top_k, params))
}

/// 组合重排(纯函数)。组合分与门槛的推导(以默认 rrf_k=60 为例):
///
/// - RRF 融合分值域:单腿 rank-r 命中 = 1/(k+r) ≤ 1/61 ≈ 0.0164;双腿 rank-1 满配
///   = 2/61 ≈ 0.0328。分值刻度由 k 决定,故权重/门槛都以满配分 2/(k+1) 为基准推导,
///   rrf_k 改配置时自动跟随,不写死绝对值。
/// - **权重 W = 0.3·2/(k+1)**(k=60 时 ≈ 0.0098):标题完全相同(重复报告的典型形态
///   ——标题抄自同一现象,正文各写各的)最多提升 30% 满配分量级。足以把「标题近似、
///   正文措辞不同」的真重复从双腿靠后位置提进前列;但检索分明显落后的命中(语义与
///   词面都不相关)不可能靠标题翻身——**检索分为主,标题相似加权**。
/// - **门槛 floor = 0.6·2/(k+1)**(k=60 时 ≈ 0.0197):孤证不立。单腿 rank-1
///   (≈0.0164)过不了门槛;双腿命中(语义+词面互证)或「单腿 + 标题相似补足」才
///   返回。查重是「是否已存在」的判断面,精确率优先:宁可空手(让 agent 放心新建),
///   不拿不相关 issue 冒充既有重复。已知代价:跨语言重复(中文草稿 vs 英文库)两腿
///   与标题相似全失,暂不召回。
fn rerank(
    hits: Vec<SearchHit>,
    draft_title: &str,
    top_k: usize,
    params: &SearchParams,
) -> Vec<DuplicateHit> {
    let unit = 2.0 / (params.rrf_k as f32 + 1.0); // 双腿 rank-1 满配分
    let w_title = 0.3 * unit;
    let floor = 0.6 * unit;
    let mut scored: Vec<(f32, DuplicateHit)> = hits
        .into_iter()
        .map(|h| {
            let title_sim = title_similarity(draft_title, &h.title);
            let combined = h.score + w_title * title_sim;
            (
                combined,
                DuplicateHit {
                    repo: h.repo,
                    number: h.number,
                    kind: h.kind,
                    title: h.title,
                    state: h.state,
                    score: h.score,
                    title_sim,
                    source: h.source,
                },
            )
        })
        .filter(|(combined, _)| *combined >= floor)
        .collect();
    // 稳定排序:组合分相同保持检索序(检索分为主的兜底语义)
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().take(top_k).map(|(_, h)| h).collect()
}
/// 标题相似度:与 FTS 索引侧同一 bigram 变换([`cjk_bigram`])切 token,取多重集
/// Dice 系数 2|A∩B|/(|A|+|B|)。中文 = 重叠双字组、英文 = 词,同一机制两边都能算;
/// 比较前统一小写(与 FTS5 unicode61 的大小写折叠语义对齐——"Crash" vs "crash"
/// 是纯噪声,不该吞掉相似度);Dice 对标题量级的短文本稳定,且对词序不敏感
/// (重复报告常改语序)。
pub fn title_similarity(a: &str, b: &str) -> f32 {
    let sa = cjk_bigram(&a.to_lowercase());
    let sb = cjk_bigram(&b.to_lowercase());
    let ta: Vec<&str> = sa.split_whitespace().collect();
    let tb: Vec<&str> = sb.split_whitespace().collect();
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let total = ta.len() + tb.len();
    // 多重集交集:逐个取走,重复 token 按出现次数计入
    let mut pool = tb;
    let mut inter = 0usize;
    for t in &ta {
        if let Some(i) = pool.iter().position(|x| x == t) {
            pool.swap_remove(i);
            inter += 1;
        }
    }
    (2.0 * inter as f32) / total as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 指纹纪律:草稿文本 = 索引侧组装的空评论退化形态,不许另造
    #[test]
    fn draft_text_is_index_assembly_no_comments() {
        assert_eq!(
            draft_text("标题", "正文", 2, 100),
            "标题\n标题\n正文",
            "基础形态:标题重复 + 正文"
        );
        assert_eq!(
            draft_text("t", "b", 2, 2000),
            build_text_with_comments("t", "b", &[], 2, 2000, COMMENT_QUOTA, PER_COMMENT_MAX),
            "必须与索引侧组装函数逐字符一致(空评论)"
        );
        // 图片降噪随组装继承(索引侧同款),URL 哈希噪声不进查询向量
        assert!(draft_text("t", "see ![x](https://a/b.png)", 1, 100).contains("[图片]"));
    }

    #[test]
    fn title_similarity_identical_is_one() {
        assert_eq!(
            title_similarity("Login redirect loop", "Login redirect loop"),
            1.0
        );
        assert_eq!(title_similarity("导出CSV中文乱码", "导出CSV中文乱码"), 1.0);
    }

    #[test]
    fn title_similarity_disjoint_or_empty_is_zero() {
        assert_eq!(
            title_similarity("Login redirect loop", "Memory leak in pool"),
            0.0
        );
        assert_eq!(title_similarity("导出CSV中文乱码", "存储过程无法展开"), 0.0);
        assert_eq!(title_similarity("", "anything"), 0.0);
        assert_eq!(title_similarity("anything", ""), 0.0);
    }

    #[test]
    fn title_similarity_partial_overlap_in_between() {
        // 英文:语序打乱、部分共词 → 严格 (0,1)
        let s = title_similarity("Crash on startup", "startup crash");
        assert!(s > 0.5 && s < 1.0, "got {s}");
        // 中文:bigram 部分重叠 → 严格 (0,1)
        let s = title_similarity("导出CSV中文乱码", "导出CSV出现乱码");
        assert!(s > 0.5 && s < 1.0, "got {s}");
    }

    fn hit(number: i64, title: &str, score: f32) -> SearchHit {
        SearchHit {
            repo: "acme/web".into(),
            number,
            kind: "issue".into(),
            title: title.into(),
            state: "open".into(),
            snippet: String::new(),
            score,
            source: "vec+fts",
        }
    }

    /// 标题加权可把「标题近似、检索分略低」的命中提到第一(重复报告的典型形态)
    #[test]
    fn title_weight_promotes_near_title_hit() {
        let hits = vec![
            hit(1, "Memory leak in pool", 0.020), // 检索分高但标题不近似
            hit(2, "Crash on startup", 0.017),    // 检索分低但标题全同
        ];
        let out = rerank(hits, "Crash on startup", 5, &SearchParams::default());
        assert_eq!(out[0].number, 2, "标题全同应提前,得到 {:?}", out);
        assert_eq!(out.len(), 2);
        // score 语义不变:返回的是检索分原值
        assert!((out[0].score - 0.017).abs() < 1e-6);
        assert!((out[0].title_sim - 1.0).abs() < 1e-6);
    }

    /// 孤证不立:单腿 rank-1(≈0.0164)+ 标题不近似 → 低于门槛,不返回
    #[test]
    fn lone_single_leg_evidence_dropped() {
        let hits = vec![hit(1, "Totally different", 1.0 / 61.0)];
        let out = rerank(hits, "Crash on startup", 5, &SearchParams::default());
        assert!(out.is_empty(), "单腿孤证不应冒充既有重复,得到 {out:?}");
    }

    /// 截断在重排之后:top_k 之外的命中丢弃
    #[test]
    fn truncates_after_rerank() {
        let hits = vec![
            hit(1, "Crash on startup", 0.030),
            hit(2, "Crash on startup", 0.028),
            hit(3, "Crash on startup", 0.026),
        ];
        let out = rerank(hits, "Crash on startup", 2, &SearchParams::default());
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].number, 1);
        assert_eq!(out[1].number, 2);
    }
}
