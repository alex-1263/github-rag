//! GithubApi:GitHub issues 拉取的 trait 抽象 + HTTP 实现。
//!
//! 分层(AGENTS 测试纪律):`parse_page` 是纯函数(单测覆盖,含 PR 过滤/字段映射);
//! `HttpGithubApi` 只做分页循环与鉴权,不掺解析逻辑。测试用假 trait 实现,零网络。

use crate::{Error, Result};

use crate::store::IssueMeta;

/// GitHub 拉取口(sync 唯一数据源)。
pub trait GithubApi {
    /// 拉取仓库全部 issue(state=all);`since`(ISO8601)存在时只拉 updated_at > since 的。
    /// 返回值不含 PR(GitHub issues API 会混入,parse 层过滤)。
    fn iter_issues(&self, repo: &str, since: Option<&str>) -> Result<Vec<IssueMeta>>;
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
    pub updated_at: String,
    /// PR 会带 pull_request 对象;issue 没有 → 过滤依据
    pub pull_request: Option<serde::de::IgnoredAny>,
}

#[derive(serde::Deserialize)]
pub struct GhLabel {
    pub name: String,
}

/// 解析一页 JSON → IssueMeta 列表(PR 过滤在此)。
pub fn parse_page(repo: &str, body: &str) -> Result<Vec<IssueMeta>> {
    let raw: Vec<GhIssue> = serde_json::from_str(body)
        .map_err(|e| Error::Io(std::io::Error::other(format!("github json: {e}"))))?;
    Ok(raw
        .into_iter()
        .filter(|i| i.pull_request.is_none())
        .map(|i| IssueMeta {
            id: i.id,
            repo: repo.to_string(),
            number: i.number,
            title: i.title.unwrap_or_default(),
            body: i.body.unwrap_or_default(),
            state: i.state,
            labels: i.labels.into_iter().map(|l| l.name).collect(),
            comments_count: i.comments,
            updated_at: i.updated_at,
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
        Self {
            token,
            client: ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(30))
                .build(),
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
}

impl GithubApi for HttpGithubApi {
    fn iter_issues(&self, repo: &str, since: Option<&str>) -> Result<Vec<IssueMeta>> {
        let mut all = Vec::new();
        let mut page = 1u32;
        loop {
            let mut url = format!(
                "https://api.github.com/repos/{repo}/issues?state=all&per_page=100&page={page}"
            );
            if let Some(s) = since {
                url.push_str(&format!("&since={s}"));
            }
            let resp = self
                .client
                .get(&url)
                .set("Authorization", &format!("Bearer {}", self.token))
                .set("Accept", "application/vnd.github+json")
                .set("User-Agent", "gh-rag")
                .call();
            let resp = match resp {
                Ok(r) => r,
                Err(ureq::Error::Status(403, r)) | Err(ureq::Error::Status(429, r)) => {
                    let body = r.into_string().unwrap_or_default();
                    return Err(Error::Io(std::io::Error::other(format!(
                        "github rate-limited (page {page}): {}",
                        body.chars().take(160).collect::<String>()
                    ))));
                }
                Err(ureq::Error::Status(code, _r)) => {
                    return Err(Error::Io(std::io::Error::other(format!(
                        "github http {code}: repo={repo}"
                    ))));
                }
                Err(e) => {
                    return Err(Error::Io(std::io::Error::other(format!("github: {e}"))));
                }
            };
            let body = resp
                .into_string()
                .map_err(|e| Error::Io(std::io::Error::other(format!("github body: {e}"))))?;
            let batch = parse_page(repo, &body)?;
            let got = batch.len();
            all.extend(batch);
            if got < 100 {
                break;
            }
            page += 1;
        }
        Ok(all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"[
      {"id":111,"number":7,"title":"连接失败","body":"postgres 报错","state":"open",
       "labels":[{"name":"bug"}],"comments":3,"updated_at":"2026-09-01T00:00:00Z"},
      {"id":222,"number":8,"title":"PR 项","body":"x","state":"open",
       "labels":[],"comments":0,"updated_at":"2026-09-02T00:00:00Z",
       "pull_request":{"url":"https://api.github.com/repos/a/b/pulls/8"}},
      {"id":333,"number":9,"title":null,"body":null,"state":"closed",
       "labels":[{"name":"p1"},{"name":"ui"}],"comments":10,"updated_at":"2026-09-03T00:00:00Z"}
    ]"#;

    #[test]
    fn parse_page_filters_pr_and_maps_fields() {
        let v = parse_page("a/b", PAGE).unwrap();
        assert_eq!(v.len(), 2, "PR 应被过滤");
        assert_eq!(v[0].number, 7);
        assert_eq!(v[0].title, "连接失败");
        assert_eq!(v[0].labels, vec!["bug"]);
        assert_eq!(v[0].comments_count, 3);
        // null 字段安全映射
        assert_eq!(v[1].title, "");
        assert_eq!(v[1].labels.len(), 2);
    }

    #[test]
    fn parse_page_empty_is_ok() {
        assert!(parse_page("a/b", "[]").unwrap().is_empty());
    }
}
