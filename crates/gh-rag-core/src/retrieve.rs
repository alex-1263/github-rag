//! 混合检索:向量召回 ∥ FTS5 召回 → RRF 融合。

use crate::embedder::Embedder;
use crate::store::IssueStore;
use crate::Result;

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub repo: String,
    pub number: i64,
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
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// FTS5 短语转义:用户原始输入包成短语,规避查询语法错误。
pub fn fts_sanitize(query: &str) -> String {
    format!("\"{}\"", query.replace('"', " "))
}

pub fn hybrid_search(
    store: &IssueStore,
    embedder: &dyn Embedder,
    query: &str,
    filter: &SearchFilter,
    top_k: usize,
    params: &SearchParams,
) -> Result<Vec<SearchHit>> {
    let repos = filter.repos.as_deref();
    let state = filter.state.as_deref();
    let labels = filter.labels.as_deref();

    // ① 向量召回:暴力点积(向量已 L2 归一化,点积 = 余弦)
    let mut vec_rank: Vec<(i64, usize)> = Vec::new();
    let candidates = store.candidates(repos, state, labels)?;
    if !candidates.is_empty() {
        let q = embedder.embed_query(query)?;
        let mut sims: Vec<(i64, f32)> = candidates
            .iter()
            .map(|(id, blob)| {
                let v = bytes_to_f32(blob);
                let dot = v.iter().zip(q.iter()).map(|(a, b)| a * b).sum::<f32>();
                (*id, dot)
            })
            .collect();
        sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        vec_rank = sims
            .iter()
            .take(params.vec_top)
            .enumerate()
            .map(|(r, (id, _))| (*id, r + 1))
            .collect();
    }

    // ② BM25 召回
    let fts_rows = store.fts_search(&fts_sanitize(query), repos, state, params.fts_top)?;
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

    let log_entries: Vec<(String, i64)> = hits.iter().map(|h| (h.repo.clone(), h.number)).collect();
    let _ = store.log_query("search_issues", query, &log_entries);
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
            Some((*id, v.iter().zip(q.iter()).map(|(a, c)| a * c).sum::<f32>()))
        })
        .collect();
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
    fn fts_sanitize_strips_quotes() {
        assert_eq!(fts_sanitize("a\"b"), "\"a b\"");
    }
}
