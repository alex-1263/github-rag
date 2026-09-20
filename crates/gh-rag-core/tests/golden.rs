//! 黄金对齐测试(两级阈值)。
//!
//! fixtures 由 Python 侧生成:`.venv/Scripts/python scripts/gen_golden.py`
//! - fp32(ort 直连):阈值 0.999 —— "Rust 能逐位复现 Python"的证明(M1)
//! - int8(fastembed):阈值 0.97 —— 量化失真的记录性下界(防预处理回归;
//!   实测稳定在 0.98x,跌破 0.97 说明 tokenizer/池化坏了,而非量化)
#![cfg(feature = "golden")]

use gh_rag_core::embedder::onnx::{Fp32Embedder, OnnxEmbedder};
use gh_rag_core::embedder::Embedder;

const GOLDEN_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/golden_embeddings.json"
);
const FP32_THRESHOLD: f32 = 0.999;
const INT8_FLOOR: f32 = 0.97;

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

fn load_golden() -> Golden {
    let raw = std::fs::read_to_string(GOLDEN_PATH).expect("run scripts/gen_golden.py first");
    serde_json::from_str(&raw).expect("valid fixture json")
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn run_alignment<E: Embedder>(embedder: &E, golden: &Golden, threshold: f32, label: &str) {
    assert!(!golden.cases.is_empty(), "fixture must contain cases");
    let mut failures = Vec::new();
    let mut min = f32::MAX;
    for case in &golden.cases {
        let out = embedder.embed_query(&case.text).expect("inference");
        let sim = cosine(&out, &case.vector);
        min = min.min(sim);
        if sim <= threshold {
            failures.push(format!("text={:?} cosine={:.6}", case.text, sim));
        }
    }
    eprintln!("[{label}] min cosine = {min:.6} (threshold {threshold})");
    assert!(
        failures.is_empty(),
        "{label} alignment failed:\n{}",
        failures.join("\n")
    );
}

#[test]
fn fp32_matches_python_bitwise() {
    let golden = load_golden();
    let embedder = Fp32Embedder::new(golden.max_seq_len)
        .expect("fp32 model under ~/.gh-rag/models/bge-m3 (Xenova)");
    run_alignment(&embedder, &golden, FP32_THRESHOLD, "fp32");
}

#[test]
fn int8_stays_above_quantization_floor() {
    let golden = load_golden();
    let embedder = OnnxEmbedder::new(golden.max_seq_len)
        .expect("int8 model under ~/.gh-rag/models/bge-m3-int8 (gpahal)");
    run_alignment(&embedder, &golden, INT8_FLOOR, "int8");
}
