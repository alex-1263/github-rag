//! M1 端到端验收:Rust 读 Python 建的真实库,检索结果与 Python 版对比。
//! 运行:cargo run -p gh-rag-core --features golden --example real_check --release
#![cfg(feature = "golden")]

use gh_rag_core::embedder::onnx::Fp32Embedder;
use gh_rag_core::retrieve::{hybrid_search, SearchFilter, SearchParams};
use gh_rag_core::store::IssueStore;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let home = std::env::var("GH_RAG_HOME").unwrap_or_else(|_| {
        #[cfg(windows)]
        {
            std::env::var("USERPROFILE").unwrap_or_default()
        }
        #[cfg(not(windows))]
        {
            std::env::var("HOME").unwrap_or_default()
        }
    });
    let db = std::path::Path::new(&home)
        .join(".gh-rag")
        .join("index.sqlite");
    let store = IssueStore::new(&db)?;
    let stats = store.repo_stats()?;
    for (repo, n, last) in &stats {
        println!("repo: {repo} issues={n} last_sync={last}");
    }
    let fp = store.manifest_get("embedding_fp")?;
    println!("manifest fp: {fp:?}");

    let emb = Fp32Embedder::new(512)?;
    let queries = [
        "连接 postgres 数据库失败报错",
        "导出查询结果到 Excel 文件",
        "table structure editor slow and unresponsive",
        "TDengine",
    ];
    for q in queries {
        let t = Instant::now();
        let hits = hybrid_search(
            &store,
            &emb,
            q,
            &SearchFilter::default(),
            4,
            &SearchParams::default(),
        )?;
        let dt = t.elapsed();
        println!("\nQ: {q}  ({}ms)", dt.as_millis());
        for h in &hits {
            println!(
                "  {:.5} [{}] {}#{} ({}) {}",
                h.score, h.source, h.repo, h.number, h.state, h.title
            );
        }
    }
    Ok(())
}
