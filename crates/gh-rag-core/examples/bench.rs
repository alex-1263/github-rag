//! 嵌入吞吐基准:Rust fp32(ort 直连)vs int8(fastembed)。
//! 运行:cargo run -p gh-rag-core --features golden --example bench --release
//! 文本规格与 Python tests/bench.py 一致(~1000 chars/条),可横向对比。
#![cfg(feature = "golden")]

use gh_rag_core::embedder::onnx::{Fp32Embedder, OnnxEmbedder};
use gh_rag_core::embedder::Embedder;
use std::time::Instant;

fn texts(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| {
            format!(
                "Benchmark issue title {i}\nBenchmark issue title {i}\n{}",
                "body text with mixed content. ".repeat(35)
            )
        })
        .collect()
}

fn bench<E: Embedder>(label: &str, e: &E, ts: &[String]) {
    let _ = e.embed_query("warmup"); // 加载不计入
    let t = Instant::now();
    let out = e.embed_texts(ts);
    let dt = t.elapsed();
    match out {
        Ok(v) => println!(
            "{label}: {} items in {:.1}s -> {:.1} items/s",
            v.len(),
            dt.as_secs_f32(),
            v.len() as f32 / dt.as_secs_f32()
        ),
        Err(e) => println!("{label}: FAILED {e}"),
    }
}

fn main() {
    let ts = texts(128);
    println!("dim check: {} chars/text", ts[0].chars().count());
    let fp32 = Fp32Embedder::new(512).expect("fp32 model");
    bench("RUST fp32 (ort)", &fp32, &ts);
    let int8 = OnnxEmbedder::new(512).expect("int8 model");
    bench("RUST int8 (fastembed)", &int8, &ts);
}
