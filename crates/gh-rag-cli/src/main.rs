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
    /// 语义检索已索引的 issues/PRs(参数与 MCP search_issues 同形)
    Search {
        /// 自然语言查询
        query: String,
        /// 限定仓库(owner/name),可多次
        #[arg(long)]
        repos: Vec<String>,
        /// 过滤状态:open | closed | all
        #[arg(long)]
        state: Option<String>,
        /// 过滤标签,可多次
        #[arg(long)]
        labels: Vec<String>,
        /// 返回条数
        #[arg(long, default_value_t = 5)]
        top_k: usize,
    },
    /// LLM 裁判检索评测:近 N 天真实查询 → top-k 逐条打分 → BeIR 指标报表
    Eval {
        /// 统计窗口天数(缺省读 [eval].days,再缺省 7)
        #[arg(long)]
        days: Option<u32>,
        /// 锚定集 JSON 路径(漂移防护:不一致 > 20% 判 invalid)
        #[arg(long)]
        anchors: Option<String>,
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
        Cmd::Search {
            query,
            repos,
            state,
            labels,
            top_k,
        } => search(&query, repos, state, labels, top_k),
        Cmd::Eval { days, anchors } => eval(days, anchors.as_deref()),
    }
}

/// CLI 检索:与 MCP search_issues 同一路径(嵌入先行不持锁 → hybrid_search_with_query),
/// query_log 记 tool=`cli-search`(不污染 eval 题库)。
fn search(
    query: &str,
    repos: Vec<String>,
    state: Option<String>,
    labels: Vec<String>,
    top_k: usize,
) -> anyhow::Result<()> {
    use gh_rag_core::api_embedder::ApiEmbedder;
    use gh_rag_core::embedder::Embedder as _;
    use gh_rag_core::retrieve::{hybrid_search_with_query, SearchFilter};

    let store = gh_rag_core::store::IssueStore::new(&default_index_path()?)?;
    let embedder = ApiEmbedder::from_env().map_err(|e| {
        anyhow::anyhow!("嵌入配置不可用:{e}(运行 `gh-rag doctor` 自检;检索语义腿需要 api key)")
    })?;
    // 嵌入先行(网络调用不持库锁),进库后做召回——与 MCP 侧一致
    let q = embedder.embed_query(query).map_err(|e| {
        anyhow::anyhow!("嵌入请求失败:{e}(检查网络与嵌入端点,可运行 `gh-rag doctor` 实测)")
    })?;
    let filter = SearchFilter {
        repos: (!repos.is_empty()).then_some(repos),
        state,
        labels: (!labels.is_empty()).then_some(labels),
    };
    let hits = hybrid_search_with_query(
        &store,
        &q,
        query,
        &filter,
        top_k,
        &gh_rag_core::config::search_params().unwrap_or_default(),
        "cli-search",
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("{}", format_hits(&hits));
    Ok(())
}

/// 人读表格:number/kind/repo/title/score/source,列宽自适应对齐(零依赖手写)。
fn format_hits(hits: &[gh_rag_core::retrieve::SearchHit]) -> String {
    use std::fmt::Write as _;
    if hits.is_empty() {
        return "(无命中)".into();
    }
    // 字符宽按 char 数近似(CJK 等宽 2 列);标题统一 1:1 截断防破表
    fn width(s: &str) -> usize {
        s.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum()
    }
    fn pad(s: &str, w: usize) -> String {
        let mut out = s.to_string();
        for _ in width(s)..w {
            out.push(' ');
        }
        out
    }
    fn fit(s: &str, w: usize) -> String {
        let mut out = String::new();
        let mut used = 0;
        for c in s.chars() {
            let cw = if c.is_ascii() { 1 } else { 2 };
            if used + cw > w.saturating_sub(1) {
                out.push('…');
                break;
            }
            out.push(c);
            used += cw;
        }
        pad(&out, w)
    }

    let nums: Vec<String> = hits.iter().map(|h| h.number.to_string()).collect();
    let w = (
        nums.iter().map(|s| s.len()).max().unwrap_or(6).max(6),
        hits.iter()
            .map(|h| width(&h.kind))
            .max()
            .unwrap_or(4)
            .max(4),
        hits.iter()
            .map(|h| width(&h.repo))
            .max()
            .unwrap_or(3)
            .max(3),
        hits.iter()
            .map(|h| width(&h.title))
            .max()
            .unwrap_or(5)
            .clamp(5, 60),
        hits.iter()
            .map(|h| format!("{:.4}", h.score).len())
            .max()
            .unwrap_or(5)
            .max(5),
        hits.iter()
            .map(|h| h.source.len())
            .max()
            .unwrap_or(6)
            .max(6),
    );
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} {} {} {} {} {}",
        pad("number", w.0),
        pad("kind", w.1),
        pad("repo", w.2),
        pad("title", w.3),
        pad("score", w.4),
        pad("source", w.5)
    );
    for (h, n) in hits.iter().zip(&nums) {
        let _ = writeln!(
            out,
            "{} {} {} {} {:.4} {}",
            pad(n, w.0),
            pad(&h.kind, w.1),
            pad(&h.repo, w.2),
            fit(&h.title, w.3),
            h.score,
            pad(h.source, w.5)
        );
    }
    out
}

fn eval(days: Option<u32>, anchors_path: Option<&str>) -> anyhow::Result<()> {
    use gh_rag_core::eval::{run_eval, Anchor, EvalParams, HttpJudge};

    let store = gh_rag_core::store::IssueStore::new(&default_index_path()?)?;
    // 评测需要两把 key:嵌入(查询向量化)+ 裁判(逐条打分)
    let ecfg = gh_rag_core::config::eval_config().map_err(|e| anyhow::anyhow!("{e}"))?;
    if ecfg.api_key.trim().is_empty() {
        anyhow::bail!(
            "裁判 api key 缺失:config.toml [eval].api_key(缺省回落 [embedding]/GH_RAG_API_KEY)"
        );
    }
    let embedder = gh_rag_core::api_embedder::ApiEmbedder::from_env()?;
    let judge = HttpJudge::new(&ecfg.base_url, &ecfg.api_key, &ecfg.judge_model);

    let anchors: Vec<Anchor> = match anchors_path {
        Some(p) => parse_anchors(&std::fs::read_to_string(p)?)?,
        None => Vec::new(),
    };

    let params = EvalParams {
        days: days.unwrap_or(ecfg.days),
        top_k: ecfg.top_k,
    };
    let report = run_eval(&store, &embedder, &judge, params, &anchors)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    // 人读报表
    println!(
        "检索评测:{} 题真实查询 × top-{} | 裁判 {}(prompt {})",
        report.per_query.len(),
        params.top_k,
        ecfg.judge_model,
        &report.prompt_hash[..8]
    );
    println!(
        "  ndcg@5 {:.3} | mrr {:.3} | hit@5 {:.3} | recall@5(池内) {:.3}",
        report.scores.ndcg_at5, report.scores.mrr, report.scores.hit_at5, report.scores.recall_at5
    );
    if let Some(reason) = &report.invalid {
        eprintln!("\n⚠ 锚定校准失效,报告不可信:{reason}\n");
    }
    println!("  抽样判例(人工 30 秒扫描):");
    for s in &report.samples {
        println!("    [{}] {}/#{} {}", s.grade, s.repo, s.number, s.title);
    }

    // JSON 落盘 ~/.gh-rag/eval-<date>.json
    let out = gh_rag_core::config::gh_rag_home().join(format!("eval-{}.json", today_iso()));
    std::fs::create_dir_all(gh_rag_core::config::gh_rag_home())?;
    std::fs::write(&out, serde_json::to_string_pretty(&report)?)?;
    println!("  报告已写入 {}", out.display());
    if report.invalid.is_some() {
        anyhow::bail!("评测报告 invalid(锚定漂移),详见上方警告");
    }
    Ok(())
}

/// 锚定集解析:裸数组,或带 `_说明`/`anchors` 字段的样例文档对象。
fn parse_anchors(raw: &str) -> anyhow::Result<Vec<gh_rag_core::eval::Anchor>> {
    #[derive(serde::Deserialize)]
    struct Doc {
        #[serde(default)]
        anchors: Vec<gh_rag_core::eval::Anchor>,
    }
    if let Ok(v) = serde_json::from_str::<Vec<gh_rag_core::eval::Anchor>>(raw) {
        return Ok(v);
    }
    serde_json::from_str::<Doc>(raw)
        .map(|d| d.anchors)
        .map_err(|e| {
            anyhow::anyhow!(
                "锚定集 JSON 解析失败: {e}(格式见 tests/fixtures/eval_anchors.example.json)"
            )
        })
}

/// 本地日期 YYYY-MM-DD(无 chrono 依赖:天序日历数 → Y-M-D)。
fn today_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    // Howard Hinnant civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
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
    use super::{format_hits, parse_anchors, sync_repos, today_iso};
    use gh_rag_core::retrieve::SearchHit;

    #[test]
    fn hits_table_columns_and_alignment() {
        let hits = vec![
            SearchHit {
                repo: "t/r".into(),
                number: 12,
                kind: "issue".into(),
                title: "连接池泄漏".into(),
                state: "open".into(),
                snippet: String::new(),
                score: 0.9137,
                source: "vec+fts",
            },
            SearchHit {
                repo: "long/repo".into(),
                number: 3,
                kind: "pr".into(),
                title: "fix leak".into(),
                state: "closed".into(),
                snippet: String::new(),
                score: 0.5,
                source: "fts",
            },
        ];
        let out = format_hits(&hits);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3, "表头+两行:\n{out}");
        // 列对齐 = 各行第 5 列(score)起点一致;标题含空格也按宽计算,不破位
        let dw = |s: &str| {
            s.chars()
                .map(|c| if c.is_ascii() { 1 } else { 2 })
                .sum::<usize>()
        };
        for l in &lines {
            assert_eq!(
                dw(l),
                dw(lines[0]),
                "显示宽一致:表头[{}]行[{}]",
                lines[0],
                l
            );
            let toks: Vec<&str> = l.split_whitespace().collect();
            let (score, source) = (toks[toks.len() - 2], toks[toks.len() - 1]);
            assert!(
                l.starts_with("number") || score.chars().all(|c| c.is_ascii_digit() || c == '.'),
                "score 列:{l}"
            );
            assert!(
                source == "source" || ["vec", "fts", "vec+fts"].contains(&source),
                "source 列:{l}"
            );
        }
        assert!(lines[1].contains("0.9137") && lines[1].contains("vec+fts"));
        assert!(lines[2].contains("long/repo") && lines[2].contains("pr"));
    }

    #[test]
    fn hits_table_empty_query_prints_placeholder() {
        assert_eq!(format_hits(&[]), "(无命中)");
    }

    #[test]
    fn repo_and_all_conflict_is_error() {
        let err = sync_repos(Some("o/r".into()), true).unwrap_err();
        assert!(err.to_string().contains("互斥"), "实际:{err}");
    }

    #[test]
    fn explicit_repo_without_all() {
        assert_eq!(sync_repos(Some("o/r".into()), false).unwrap(), vec!["o/r"]);
    }

    #[test]
    fn anchors_bare_array_parses() {
        let v =
            parse_anchors(r#"[{"query":"q","repo":"o/r","number":1,"expected_grade":2}]"#).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].expected_grade, 2);
    }

    #[test]
    fn anchors_example_doc_parses_and_ignores_comment_keys() {
        let v = parse_anchors(
            r#"{"_说明":"格式样例","_示例条目":{"query":"q"},"anchors":[{"query":"q","repo":"o/r","number":7,"expected_grade":0,"note":"n"}]}"#,
        )
        .unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].number, 7);
    }

    #[test]
    fn anchors_bad_json_rejected() {
        assert!(parse_anchors("not json").is_err());
    }

    #[test]
    fn today_iso_is_iso_date() {
        let t = today_iso();
        assert_eq!(t.len(), 10);
        let b: Vec<&str> = t.split('-').collect();
        assert_eq!(b.len(), 3);
        assert!(b[0].parse::<i32>().unwrap() > 2020);
        assert!((1..=12).contains(&b[1].parse::<u32>().unwrap()));
        assert!((1..=31).contains(&b[2].parse::<u32>().unwrap()));
    }
}
