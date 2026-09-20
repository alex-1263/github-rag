//! 黄金对齐测试(TDD 红 → M1 变绿)。
//!
//! fixtures 由 Python 侧生成:`.venv/Scripts/python scripts/gen_golden.py`
//! 阈值 0.999 为硬约束,见 AGENTS.md 禁忌清单。
#![cfg(feature = "golden")]

use gh_rag_core::embedder::onnx::OnnxEmbedder;
use gh_rag_core::embedder::Embedder;

const GOLDEN_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/golden_embeddings.json"
);
const THRESHOLD: f32 = 0.999;

#[derive(serde::Deserialize)]
struct Golden {
    model: String,
    max_seq_len: usize,
    cases: Vec<Case>,
}

#[derive(serde::Deserialize)]
struct Case {
    text: String,
    vector: Vec<f32>,
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    // fixtures 与实现输出均已 L2 归一化,点积即余弦
    dot
}

#[test]
fn rust_onnx_output_matches_python_golden() {
    let raw = std::fs::read_to_string(GOLDEN_PATH).expect("run scripts/gen_golden.py first");
    let golden: Golden = serde_json::from_str(&raw).expect("valid fixture json");

    assert!(!golden.cases.is_empty(), "fixture must contain cases");

    let embedder = OnnxEmbedder::new(&golden.model, golden.max_seq_len);
    let mut failures = Vec::new();

    for case in &golden.cases {
        let out = embedder
            .embed_query(&case.text)
            .expect("inference must not error");
        let sim = cosine(&out, &case.vector);
        if sim <= THRESHOLD {
            failures.push(format!("text={:?} cosine={:.6}", case.text, sim));
        }
    }

    assert!(
        failures.is_empty(),
        "golden alignment failed (threshold {}):\n{}",
        THRESHOLD,
        failures.join("\n")
    );
}
