//! 混合检索:向量召回 ∥ FTS5 召回 → RRF 融合。
//! M2 实现;M1 阶段仅占位保持 workspace 编译。

/// RRF 融合(纯函数,M1 即可测试)。
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
}
