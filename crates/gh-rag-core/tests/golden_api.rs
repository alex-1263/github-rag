//! API 嵌入的黄金对齐验证(网络测试,手动运行):
//! GH_RAG_API_KEY=sk-xxx cargo test -p gh-rag-core --features api,golden --test golden_api -- --nocapture --ignored
//!
//! 回答的问题:硅基流动托管的 bge-m3 与本地 sentence-transformers 输出是否足够一致。
//! ≥0.999 → API 库和本地库可视为同一空间(仍建议单一实现);0.99x → 必须单一实现全库重建。
#![cfg(feature = "api")]

use gh_rag_core::api_embedder::ApiEmbedder;
use gh_rag_core::embedder::Embedder;

const GOLDEN_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/golden_embeddings.json"
);

#[derive(serde::Deserialize)]
struct Golden {
    cases: Vec<Case>,
}

#[derive(serde::Deserialize)]
struct Case {
    text: String,
    vector: Vec<f32>,
}

#[test]
#[ignore = "需要 GH_RAG_API_KEY 与网络,手动执行"]
fn api_bge_m3_alignment_vs_local_golden() {
    let raw = std::fs::read_to_string(GOLDEN_PATH).expect("run scripts/gen_golden.py first");
    let golden: Golden = serde_json::from_str(&raw).unwrap();

    let emb = ApiEmbedder::from_env().expect("GH_RAG_API_KEY");
    let mut min = f32::MAX;
    let mut worst = String::new();
    for case in &golden.cases {
        let out = emb.embed_query(&case.text).expect("api call");
        let sim: f32 = out.iter().zip(&case.vector).map(|(a, b)| a * b).sum();
        if sim < min {
            min = sim;
            worst = case.text.clone();
        }
    }
    println!("[api bge-m3] min cosine = {min:.6} (worst: {worst:?})");
    // 记录性断言:API 与本地同模型应高度一致;跌破 0.99 说明服务端实现差异过大
    assert!(min > 0.99, "api alignment too low: {min}");
}
