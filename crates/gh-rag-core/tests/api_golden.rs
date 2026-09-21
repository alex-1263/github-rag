//! API 黄金对齐测试:ApiEmbedder 输出 vs Python 参考向量(fixtures)。
//!
//! fixtures 由 Python 侧(已退役)生成后冻结于 `tests/fixtures/golden_embeddings.json`,
//! 作为 bge-m3 fp32 参考向量的永久基准。本地实现退役后,该测试继续守护
//! **API 嵌入与既有索引同空间**——阈值 0.999(实测稳定 0.9999)。
//! 跑法:`GH_RAG_API_KEY=xxx cargo test -p gh-rag-core --features golden`
#![cfg(feature = "golden")]

use gh_rag_core::api_embedder::ApiEmbedder;
use gh_rag_core::embedder::Embedder;

const GOLDEN_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/golden_embeddings.json"
);
const API_THRESHOLD: f32 = 0.999;

#[derive(serde::Deserialize)]
struct Golden {
    max_seq_len: usize,
    cases: Vec<Case>,
}

#[derive(serde::Deserialize)]
struct Case {
    text: String,
    vector: Vec<f32>,
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut na, mut nb) = (0f32, 0f32, 0f32);
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    dot / (na.sqrt() * nb.sqrt())
}

#[test]
fn api_embeddings_align_with_frozen_golden() {
    if std::env::var("GH_RAG_API_KEY").is_err() {
        eprintln!("跳过:未设置 GH_RAG_API_KEY(API 黄金对齐需在线)");
        return;
    }
    let raw = std::fs::read_to_string(GOLDEN_PATH).expect("fixtures 存在");
    let golden: Golden = serde_json::from_str(&raw).expect("fixtures 可解析");
    assert!(golden.cases.len() >= 10, "fixtures 至少 10 条");

    let embedder = ApiEmbedder::from_env().expect("ApiEmbedder 构造");
    let texts: Vec<String> = golden.cases.iter().map(|c| c.text.clone()).collect();
    // embed_texts 输出小端字节;对齐测试用 f32 视图
    let bytes = embedder.embed_texts(&texts).expect("API 批量嵌入");
    assert_eq!(bytes.len(), golden.cases.len());

    let mut worst = f32::MAX;
    for (i, case) in golden.cases.iter().enumerate() {
        let v: Vec<f32> = bytes[i]
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        assert_eq!(v.len(), case.vector.len(), "case {i} 维度不一致");
        let sim = cosine(&v, &case.vector);
        worst = worst.min(sim);
        assert!(
            sim > API_THRESHOLD,
            "case {i} 余弦 {sim:.6} <= {API_THRESHOLD}:{}",
            case.text.chars().take(30).collect::<String>()
        );
    }
    eprintln!("API 黄金对齐:最差余弦 {worst:.6}(阈值 {API_THRESHOLD})");
}
