//! CLI 薄壳:参数解析 → core。业务逻辑为零(AGENTS.md 架构规则)。

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "gh-rag", version, about = "semantic memory over GitHub issues")]
struct Args {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(clap::Subcommand, Debug)]
enum Cmd {
    /// 环境自检:打印嵌入配置解析结果并实测一次嵌入
    Doctor,
}

fn main() -> anyhow::Result<()> {
    match Args::parse().cmd {
        Some(Cmd::Doctor) | None => doctor(),
    }
}

fn doctor() -> anyhow::Result<()> {
    use gh_rag_core::config::{gh_rag_home, resolve};

    println!("gh-rag doctor");
    let home = gh_rag_home();
    println!("  home: {}", home.display());
    let cfg = resolve().map_err(|e| anyhow::anyhow!("{e}"))?;
    let key_origin = if std::env::var("GH_RAG_API_KEY").is_ok() {
        "env GH_RAG_API_KEY"
    } else if cfg.key.is_empty() {
        "(missing!)"
    } else {
        "config.toml embedding.api_key"
    };
    use gh_rag_core::embedder::Embedder as _;
    println!("  endpoint: {}", cfg.base);
    println!("  model:    {}", cfg.model);
    println!("  api key:  {}", key_origin);

    if cfg.key.is_empty() {
        anyhow::bail!("api key missing — cannot probe endpoint");
    }

    let embedder = gh_rag_core::api_embedder::ApiEmbedder::from_env()?;
    let v = embedder
        .embed_query("doctor 探活")
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("  probe:    ok ({} dims)", v.len());
    println!("all good.");
    Ok(())
}
