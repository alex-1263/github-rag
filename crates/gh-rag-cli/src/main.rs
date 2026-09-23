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
    /// 质量报表:近 N 天 query_log 统计(总量/去重/top10/follow_up 率/工具分布)
    Report {
        /// 统计窗口天数
        #[arg(long, default_value_t = 30)]
        days: u32,
    },
    /// 环境自检:打印嵌入配置解析结果并实测一次嵌入
    Doctor,
    /// 导出骨架库(向量+元数据,无全文;分发形态)
    Export {
        /// 导出骨架库(唯一形态,保留此参数以明示意图)
        #[arg(long)]
        skeleton: bool,
        /// 输出文件路径
        #[arg(short, long)]
        output: String,
    },
    /// 装载骨架库(URL 或本地文件;指纹校验+安全导入),随后 sync 补全文
    Fetch {
        /// 骨架库 URL 或本地路径(.gz 自动解压)
        #[arg(long)]
        from: String,
    },
}

fn main() -> anyhow::Result<()> {
    match Args::parse().cmd.unwrap_or(Cmd::Status) {
        Cmd::Doctor => doctor(),
        Cmd::Export { skeleton, output } => export(skeleton, output),
        Cmd::Fetch { from } => fetch(from),
        Cmd::Status => status(),
        Cmd::Report { days } => report(days),
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

fn report(days: u32) -> anyhow::Result<()> {
    let store = gh_rag_core::store::IssueStore::new(&default_index_path()?)?;
    let r = store
        .query_report(days)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("query_log 报表(近 {} 天)", r.days);
    println!("  查询总数:{}", r.total);
    println!("  去重查询:{}", r.unique);
    println!("  follow_up 率:{:.1}%", r.follow_up_rate * 100.0);
    println!("  top 高频查询:");
    for (q, n) in &r.top_queries {
        println!("    {n:>4} × {q}");
    }
    println!("  按工具分布:");
    for (tool, n) in &r.by_tool {
        println!("    {n:>4} × {tool}");
    }
    Ok(())
}

fn sync(repo: Option<String>, all: bool) -> anyhow::Result<()> {
    use gh_rag_core::github::HttpGithubApi;
    use gh_rag_core::sync::sync_repo;

    let repos = sync_repos(repo, all)?;

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
        let home = gh_rag_core::config::gh_rag_home();
        let r = sync_repo(&github, &store, &embedder, &repo, &params, &home)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        println!(
            "  拉取 {} 条 / 评论 {}:嵌入 {},跳过 {}",
            r.fetched, r.comments_fetched, r.embedded, r.skipped
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

fn export(skeleton: bool, output: String) -> anyhow::Result<()> {
    if !skeleton {
        anyhow::bail!("--skeleton 为唯一导出形态(全文导出涉版权红线,故意不提供)");
    }
    let store = gh_rag_core::store::IssueStore::new(&default_index_path()?)?;
    let n = gh_rag_core::skeleton::export_skeleton(&store, std::path::Path::new(&output))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("骨架已导出 {n} 条 → {output}");
    println!("分发前建议 gzip:骨架库不含正文/评论全文,可安全分发");
    Ok(())
}

fn fetch(from: String) -> anyhow::Result<()> {
    let home = gh_rag_core::config::gh_rag_home();

    // 网络下载才产生临时件;本地路径绝不触碰用户源文件
    let is_download = from.starts_with("http://") || from.starts_with("https://");
    let tmp = home.join("fetch-download.tmp");
    let src = if is_download {
        gh_rag_core::skeleton::download_to(&from, &tmp).map_err(|e| anyhow::anyhow!("{e}"))?
    } else {
        std::path::PathBuf::from(&from)
    };
    let db_path =
        gh_rag_core::skeleton::gunzip_if_needed(&src).map_err(|e| anyhow::anyhow!("{e}"))?;

    // 指纹与 sync 落库同源组装(full_fingerprint 唯一真相源):无 key 也能 fetch,
    // 因为导入本身只比对指纹,不需要发起嵌入请求
    let cfg = gh_rag_core::config::resolve().map_err(|e| anyhow::anyhow!("{e}"))?;
    let base_fp = match cfg.dimensions {
        // 与 ApiEmbedder::fingerprint 同规则:维度进 model 段
        Some(d) => format!("{}[dim={}]", cfg.model, d),
        None => cfg.model.clone(),
    };
    let base_fp = format!("{base_fp}|api|len=512");
    let (title_repeats, body_max_chars) = gh_rag_core::config::text_params()?;
    let expect_fp = gh_rag_core::sync::full_fingerprint(
        &base_fp,
        &gh_rag_core::sync::SyncParams {
            batch_size: 0,
            batch_interval: Duration::ZERO,
            title_repeats,
            body_max_chars,
        },
    );

    let store = open_or_create_index()?;
    let r = gh_rag_core::skeleton::import_skeleton(&db_path, &store, &expect_fp)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!(
        "骨架装载完成:issue/pr {} 条,向量 {} 条,指纹 {}",
        r.issues,
        r.vectors,
        r.fingerprint.unwrap_or_default()
    );
    // 清理仅限自己创建的临时下载件及其解压产物;用户本地源文件永不删除
    if is_download {
        if src != db_path {
            let _ = std::fs::remove_file(&db_path);
        }
        let _ = std::fs::remove_file(&tmp);
    }
    println!("下一步:gh-rag sync --all 补全文(内容哈希对齐,向量零重嵌)");
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

/// sync 的仓库清单解析(独立函数便于单测):
/// repo 与 --all 同给 = 参数冲突,显式报错(不再静默吞掉位置参数)。
fn sync_repos(repo: Option<String>, all: bool) -> anyhow::Result<Vec<String>> {
    if all && repo.is_some() {
        anyhow::bail!("`gh-rag sync <repo>` 与 --all 互斥,只能给其一");
    }
    match repo {
        Some(r) => Ok(vec![r]),
        None => gh_rag_core::config::repos()?.ok_or_else(|| {
            anyhow::anyhow!("config.toml 未配置 repos;或显式 `gh-rag sync owner/name`")
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::sync_repos;

    #[test]
    fn repo_and_all_conflict_is_error() {
        let err = sync_repos(Some("o/r".into()), true).unwrap_err();
        assert!(err.to_string().contains("互斥"), "实际:{err}");
    }

    #[test]
    fn explicit_repo_without_all() {
        assert_eq!(sync_repos(Some("o/r".into()), false).unwrap(), vec!["o/r"]);
    }
}
