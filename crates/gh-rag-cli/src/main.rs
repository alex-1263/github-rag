//! CLI 薄壳:参数解析 → core。业务逻辑为零(AGENTS.md 架构规则)。

use clap::Parser;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(name = "gh-rag", version, about = "semantic memory over GitHub issues")]
struct Args {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(clap::Subcommand, Debug)]
enum Cmd {
    /// 同步仓库:全量首建或增量(内容 hash 跳过未变更条目)
    Sync {
        /// 仓库(owner/name);缺省 --all 读 config
        repo: Option<String>,
        /// 同步 config.toml repos 列表的全部仓库
        #[arg(long)]
        all: bool,
    },
    /// 索引状态:各仓库条数与游标
    Status,
    /// 环境自检:打印嵌入配置解析结果并实测一次嵌入
    Doctor,
}

fn main() -> anyhow::Result<()> {
    match Args::parse().cmd.unwrap_or(Cmd::Status) {
        Cmd::Doctor => doctor(),
        Cmd::Status => status(),
        Cmd::Sync { repo, all } => sync(repo, all),
    }
}

fn status() -> anyhow::Result<()> {
    let store = gh_rag_core::store::IssueStore::new(&default_index_path()?)?;
    for (repo, count, last) in store.repo_stats().map_err(|e| anyhow::anyhow!("{e}"))? {
        println!("{repo:40} {count:>6} 条   上次同步 {last}");
    }
    Ok(())
}

fn sync(repo: Option<String>, all: bool) -> anyhow::Result<()> {
    use gh_rag_core::github::HttpGithubApi;
    use gh_rag_core::sync::sync_repo;

    let repos: Vec<String> = match repo {
        Some(r) if !all => vec![r],
        _ => gh_rag_core::config::repos()?.ok_or_else(|| {
            anyhow::anyhow!("config.toml 未配置 repos;或显式 `gh-rag sync owner/name`")
        })?,
    };

    let github = HttpGithubApi::from_env_config()?;
    let store = open_or_create_index()?;
    let embedder = gh_rag_core::api_embedder::ApiEmbedder::from_env()?;
    let (title_repeats, body_max_chars) = gh_rag_core::config::text_params()?;
    let params = gh_rag_core::sync::SyncParams {
        batch_size: 64,
        batch_interval: Duration::from_millis(gh_rag_core::config::batch_interval_ms()?),
        title_repeats,
        body_max_chars,
    };

    for repo in repos {
        println!("sync {repo} …");
        let r = sync_repo(&github, &store, &embedder, &repo, &params)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        println!(
            "  拉取 {} 条:嵌入 {},跳过(未变更){}",
            r.fetched, r.embedded, r.skipped
        );
    }
    Ok(())
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

fn default_index_path() -> anyhow::Result<std::path::PathBuf> {
    Ok(gh_rag_core::config::gh_rag_home().join("index.sqlite"))
}

fn open_or_create_index() -> anyhow::Result<gh_rag_core::store::IssueStore> {
    let p = default_index_path()?;
    if p.exists() {
        gh_rag_core::store::IssueStore::new(&p).map_err(|e| anyhow::anyhow!("{e}"))
    } else {
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        gh_rag_core::store::IssueStore::create_fixture(&p).map_err(|e| anyhow::anyhow!("{e}"))
    }
}
