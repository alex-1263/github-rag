//! GithubApi:GitHub issues 拉取的 trait 抽象 + HTTP 实现。
//!
//! 分层(AGENTS 测试纪律):`parse_page` / `parse_comments` 是纯函数(单测覆盖);
//! `HttpGithubApi` 只做分页循环与鉴权,不掺解析逻辑。测试用假 trait 实现,零网络。

use crate::{Error, Result};

use crate::store::IssueMeta;
use std::time::Duration;

/// GitHub 拉取口(sync 唯一数据源)。
pub trait GithubApi {
    /// 拉取仓库全部 issue(state=all);`since`(ISO8601)存在时只拉 updated_at >= since 的。
    fn iter_issues(&self, repo: &str, since: Option<&str>) -> Result<Vec<IssueMeta>>;

    /// 流式分页:每拉到一页即回调(页内限流在实现层),调用方逐页持久化,中途失败不丢已拉数据。
    /// 默认实现 = iter_issues 一次性回调(假 API 无需感知)。
    fn issues_pages(
        &self,
        repo: &str,
        since: Option<&str>,
        f: &mut dyn FnMut(Vec<IssueMeta>) -> Result<()>,
    ) -> Result<()> {
        f(self.iter_issues(repo, since)?)
    }

    /// 仓库级评论(cursor 分页);`since` 增量游标。默认空(测试假 API 按需覆盖)。
    fn iter_comments(
        &self,
        _repo: &str,
        _since: Option<&str>,
    ) -> Result<Vec<crate::raw::RawComment>> {
        Ok(Vec::new())
    }
}

/// 单条 GitHub issues API JSON 的(部分)形状。
#[derive(serde::Deserialize)]
pub struct GhIssue {
    pub id: i64,
    pub number: i64,
    pub title: Option<String>,
    pub body: Option<String>,
    pub state: String,
    #[serde(default)]
    pub labels: Vec<GhLabel>,
    #[serde(default)]
    pub comments: i64,
    pub created_at: Option<String>,
    pub user: Option<GhUser>,
    pub updated_at: String,
    /// PR 会带 pull_request 对象;issue 没有 → 过滤依据
    pub pull_request: Option<serde::de::IgnoredAny>,
}

#[derive(serde::Deserialize)]
pub struct GhLabel {
    pub name: String,
}

/// issue 评论(聚合进嵌入文本;author 保留语义角色)。
#[derive(Clone, Debug, PartialEq)]
pub struct Comment {
    pub author: String,
    pub body: String,
}

#[derive(serde::Deserialize)]
struct GhComment {
    id: i64,
    user: Option<GhUser>,
    body: Option<String>,
    issue_url: Option<String>,
    created_at: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct GhUser {
    pub login: String,
}

/// 解析一页 JSON → IssueMeta 列表(PR 过滤在此;评论由 HTTP 层按需补拉)。
pub fn parse_page(repo: &str, body: &str) -> Result<Vec<IssueMeta>> {
    let raw: Vec<GhIssue> = serde_json::from_str(body)
        .map_err(|e| Error::Io(std::io::Error::other(format!("github json: {e}"))))?;
    Ok(raw
        .into_iter()
        .map(|i| IssueMeta {
            id: i.id,
            repo: repo.to_string(),
            number: i.number,
            kind: if i.pull_request.is_some() {
                "pr"
            } else {
                "issue"
            }
            .to_string(),
            title: i.title.unwrap_or_default(),
            body: i.body.unwrap_or_default(),
            state: i.state,
            labels: i.labels.into_iter().map(|l| l.name).collect(),
            comments_count: i.comments,
            comments: None,
            author: i.user.map(|u| u.login).unwrap_or_default(),
            created_at: i.created_at.unwrap_or_default(),
            updated_at: i.updated_at,
        })
        .collect())
}

/// issue_url 尾段提取编号:.../repos/o/r/issues/123 → 123
fn number_from_issue_url(url: &str) -> Option<i64> {
    url.rsplit('/').next()?.parse().ok()
}

/// 解析仓库级评论页 JSON → RawComment(带 id/归属/时间);空评论过滤,时间序即返回序。
pub fn parse_comments_page(body: &str) -> Result<Vec<crate::raw::RawComment>> {
    let raw: Vec<GhComment> = serde_json::from_str(body)
        .map_err(|e| Error::Io(std::io::Error::other(format!("github comments json: {e}"))))?;
    Ok(raw
        .into_iter()
        .filter_map(|c| {
            let body = c.body?;
            let number = number_from_issue_url(c.issue_url.as_deref()?)?;
            if body.trim().is_empty() {
                None
            } else {
                Some(crate::raw::RawComment {
                    id: c.id,
                    issue_number: number,
                    author: c.user.map(|u| u.login).unwrap_or_default(),
                    body,
                    created_at: c.created_at.unwrap_or_default(),
                })
            }
        })
        .collect())
}

/// HTTP 实现:ureq + 分页。token 依次取 GH_RAG_TOKEN > config.toml token > `gh auth token`。
pub struct HttpGithubApi {
    token: String,
    client: ureq::Agent,
}

impl HttpGithubApi {
    pub fn from_token(token: String) -> Self {
        // 中国网络直连 api.github.com 长拉取易被掐:支持 HTTPS_PROXY/ALL_PROXY(与 skeleton 下载同款)
        let mut builder = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(30));
        if let Some(u) = crate::skeleton::proxy_url_from_env(
            std::env::var("HTTPS_PROXY").ok().as_deref(),
            std::env::var("ALL_PROXY").ok().as_deref(),
        ) {
            if let Ok(p) = ureq::Proxy::new(&u) {
                builder = builder.proxy(p);
            } else {
                eprintln!("[gh-rag] 代理配置无法解析({u}),回退直连");
            }
        }
        Self {
            token,
            client: builder.build(),
        }
    }

    /// 按 AGENTS 约定的 token 顺序解析。
    pub fn from_env_config() -> Result<Self> {
        let token = if let Ok(t) = std::env::var("GH_RAG_TOKEN") {
            t
        } else if let Some(t) = crate::config::github_token()? {
            t
        } else {
            let out = std::process::Command::new("gh")
                .args(["auth", "token"])
                .output()
                .map_err(|e| Error::Io(std::io::Error::other(format!("gh auth token: {e}"))))?;
            if !out.status.success() {
                return Err(Error::Io(std::io::Error::other(
                    "no GitHub token: set GH_RAG_TOKEN or run `gh auth login`",
                )));
            }
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        Ok(Self::from_token(token))
    }

    fn get(&self, url: &str) -> Result<(String, Option<String>)> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            throttle();
            let resp = self
                .client
                .get(url)
                .set("Authorization", &format!("Bearer {}", self.token))
                .set("Accept", "application/vnd.github+json")
                .set("User-Agent", "gh-rag")
                .call();
            match resp {
                Ok(r) => {
                    let next = link_next(r.header("Link"));
                    // 剩余额度告急:sleep 至重置时刻(审查③:不尊重 x-ratelimit)
                    let remaining = r
                        .header("x-ratelimit-remaining")
                        .and_then(|s| s.parse().ok());
                    let reset = r.header("x-ratelimit-reset").and_then(|s| s.parse().ok());
                    if let (Some(remaining), Some(reset)) = (remaining, reset) {
                        if let Some(wait) = ratelimit_wait(remaining, reset, now_unix()) {
                            std::thread::sleep(wait);
                        }
                    }
                    // 读体失败(连接成但流被掐)同样退避重试——整个 Ok 分支重来
                    match r.into_string() {
                        Ok(body) => return Ok((body, next)),
                        Err(e) if attempt <= 3 => {
                            let secs = 2u64 << (attempt - 1);
                            eprintln!("[gh-rag] 读体中断,{secs}s 后重试(第 {attempt} 次): {e}");
                            std::thread::sleep(std::time::Duration::from_secs(secs));
                            continue;
                        }
                        Err(e) => {
                            return Err(Error::Io(std::io::Error::other(format!(
                                "github body: {e}"
                            ))))
                        }
                    }
                }
                Err(ureq::Error::Status(403, r)) | Err(ureq::Error::Status(429, r)) => {
                    let retry_after = r.header("Retry-After").and_then(parse_retry_after);
                    let body = r.into_string().unwrap_or_default();
                    match retry_after {
                        Some(d) if attempt <= 3 => {
                            std::thread::sleep(d);
                            continue;
                        }
                        _ => {
                            return Err(Error::Io(std::io::Error::other(format!(
                                "github rate-limited: {}",
                                body.chars().take(160).collect::<String>()
                            ))))
                        }
                    }
                }
                Err(ureq::Error::Status(code, _)) => Err(Error::Io(std::io::Error::other(
                    format!("github http {code}: {url}"),
                )))?,
                // 传输层错误(墙抖/代理瞬断/RST):退避重试而非整轮报废——raw 层有断点,
                // 进程内重试把网络毛刺变成几秒延迟
                Err(e) if attempt <= 3 => {
                    let secs = 2u64 << (attempt - 1); // 2s, 4s, 8s
                    eprintln!("[gh-rag] 网络瞬断,{secs}s 后重试(第 {attempt} 次)");
                    std::thread::sleep(std::time::Duration::from_secs(secs));
                    continue;
                }
                Err(e) => Err(Error::Io(std::io::Error::other(format!("github: {e}"))))?,
            }
        }
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 请求级最小间隔(进程内静态节流,审查③:无翻页间隔)。
const MIN_REQUEST_INTERVAL: Duration = Duration::from_millis(200);

/// 进程内静态节流:距上次请求不足 200ms 则补睡。
fn throttle() {
    static LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(t) = *last {
        let elapsed = t.elapsed();
        if elapsed < MIN_REQUEST_INTERVAL {
            std::thread::sleep(MIN_REQUEST_INTERVAL - elapsed);
        }
    }
    *last = Some(std::time::Instant::now());
}

/// 解析 Retry-After 头(秒数形式);负数/非数字返回 None。
pub fn parse_retry_after(s: &str) -> Option<Duration> {
    let secs: i64 = s.trim().parse().ok()?;
    if secs < 0 {
        None
    } else {
        Some(Duration::from_secs(secs as u64))
    }
}

/// 剩余额度 < 200 时计算需等待时长(reset 时刻由调用方传入 now 判定,不取系统时间)。
pub fn ratelimit_wait(remaining: u64, reset_unix: u64, now_unix: u64) -> Option<Duration> {
    if remaining >= 200 || reset_unix <= now_unix {
        None
    } else {
        Some(Duration::from_secs(reset_unix - now_unix))
    }
}

/// cursor 分页:GitHub 对大数据集禁用 page>100(HTTP 422),
/// 唯一可靠姿势 = 跟随响应头 `Link: <...>; rel="next"` 游标。
fn link_next(link: Option<&str>) -> Option<String> {
    let link = link?;
    for part in link.split(',') {
        let part = part.trim();
        if part.ends_with(r#"; rel="next""#) {
            let u = part.strip_prefix('<')?.strip_suffix(r#">; rel="next""#)?;
            return Some(u.to_string());
        }
    }
    None
}

impl GithubApi for HttpGithubApi {
    fn iter_issues(&self, repo: &str, since: Option<&str>) -> Result<Vec<IssueMeta>> {
        let mut all = Vec::new();
        self.issues_pages(repo, since, &mut |page| {
            all.extend(page);
            Ok(())
        })?;
        Ok(all)
    }

    fn issues_pages(
        &self,
        repo: &str,
        since: Option<&str>,
        f: &mut dyn FnMut(Vec<IssueMeta>) -> Result<()>,
    ) -> Result<()> {
        let mut url = format!("https://api.github.com/repos/{repo}/issues?state=all&per_page=100");
        if let Some(s) = since {
            url.push_str(&format!("&since={s}"));
        }
        let mut next = Some(url);
        while let Some(u) = next {
            let (body, n) = self.get(&u)?;
            let batch = parse_page(repo, &body)?;
            f(batch)?;
            next = n;
        }
        Ok(())
    }

    fn iter_comments(
        &self,
        repo: &str,
        since: Option<&str>,
    ) -> Result<Vec<crate::raw::RawComment>> {
        let mut url = format!("https://api.github.com/repos/{repo}/issues/comments?per_page=100");
        if let Some(s) = since {
            url.push_str(&format!("&since={s}"));
        }
        let mut all = Vec::new();
        let mut next = Some(url);
        while let Some(u) = next {
            let (body, n) = self.get(&u)?;
            let batch = parse_comments_page(&body)?;
            all.extend(batch);
            next = n;
        }
        Ok(all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"[
      {"id":111,"number":7,"title":"连接失败","body":"postgres 报错","state":"open",
       "labels":[{"name":"bug"}],"comments":1,"updated_at":"2026-09-01T00:00:00Z",
       "user":{"login":"alice"},"created_at":"2026-08-30T00:00:00Z"},
      {"id":222,"number":8,"title":"PR 项","body":"x","state":"open",
       "labels":[],"comments":0,"updated_at":"2026-09-02T00:00:00Z",
       "pull_request":{"url":"https://api.github.com/repos/a/b/pulls/8"}}
    ]"#;

    #[test]
    fn parse_page_filters_pr_and_maps_fields() {
        let v = parse_page("a/b", PAGE).unwrap();
        assert_eq!(v.len(), 2, "PR 保留入库(kind 区分)");
        assert_eq!(v[0].number, 7);
        assert_eq!(v[0].kind, "issue");
        assert_eq!(v[0].comments_count, 1);
        assert_eq!(v[0].comments, None, "评论由 sync 层从仓库级端点聚合");
        assert_eq!(v[1].kind, "pr", "PR 标记");
        assert_eq!(v[0].labels, vec!["bug"]);
        assert_eq!(v[0].author, "alice", "author(login) 必须入库");
        assert_eq!(v[0].created_at, "2026-08-30T00:00:00Z");
        assert_eq!(v[1].author, "", "缺 user 字段容忍为空");
    }
    #[test]
    fn parse_comments_page_groups_by_issue_url() {
        let raw = r#"[
          {"id":1,"user":{"login":"alice"},"body":"复现步骤:连接串带空格",
           "issue_url":"https://api.github.com/repos/a/b/issues/1028","created_at":"2026-09-01T00:00:00Z"},
          {"id":2,"user":{"login":"bob"},"body":"   ",
           "issue_url":"https://api.github.com/repos/a/b/issues/1","created_at":"2026-09-02T00:00:00Z"},
          {"id":3,"user":null,"body":"bot 留言","issue_url":"https://api.github.com/repos/a/b/issues/2","created_at":"2026-09-03T00:00:00Z"}
        ]"#;
        let v = parse_comments_page(raw).unwrap();
        assert_eq!(v.len(), 2, "空白评论过滤");
        assert_eq!(v[0].issue_number, 1028, "归属编号提取");
        assert_eq!(v[0].author, "alice");
        assert_eq!(v[1].author, "");
    }

    #[test]
    fn parse_retry_after_reads_seconds_only() {
        assert_eq!(parse_retry_after("30"), Some(Duration::from_secs(30)));
        assert_eq!(parse_retry_after("0"), Some(Duration::ZERO));
        assert_eq!(parse_retry_after("-5"), None, "负数非法");
        assert_eq!(parse_retry_after("abc"), None);
        assert_eq!(parse_retry_after(""), None);
    }

    #[test]
    fn ratelimit_wait_only_when_remaining_low() {
        // now 由调用方传入,不取系统时间
        assert_eq!(
            ratelimit_wait(150, 1_800, 1_000),
            Some(Duration::from_secs(800))
        );
        assert_eq!(
            ratelimit_wait(199, 1_800, 1_000),
            Some(Duration::from_secs(800))
        );
        assert_eq!(ratelimit_wait(200, 1_800, 1_000), None, "余量充足不等待");
        assert_eq!(ratelimit_wait(10, 500, 1_000), None, "reset 已过不等待");
    }

    #[test]
    fn link_next_extracts_cursor() {
        let h = r#"<https://api.github.com/repositories/1/issues?after=abc&per_page=100>; rel="next", <https://api.github.com/repositories/1/issues?after=zzz>; rel="last""#;
        assert_eq!(
            link_next(Some(h)).as_deref(),
            Some("https://api.github.com/repositories/1/issues?after=abc&per_page=100")
        );
        assert_eq!(link_next(Some(r#"<x>; rel="last""#)), None);
        assert_eq!(link_next(None), None);
    }
}
