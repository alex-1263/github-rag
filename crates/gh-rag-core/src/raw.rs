//! raw 原始层:GitHub 抓取数据的本地落盘(JSONL),重建索引的零 API 数据源。
//!
//! `~/.gh-rag/raw/{repo 转义}/issues.jsonl` + `comments.jsonl`
//! - issues:按 number upsert(updated_at 最新者胜)
//! - comments:按 id 去重追加(评论的增删没有可靠增量信号,拉到即真相)
//! - 重建索引(换模型/改聚合参数)只读 raw,不再打 GitHub API

use crate::github::Comment;
use crate::store::IssueMeta;
use crate::{Error, Result};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

/// raw 层的 issue 记录(与 IssueMeta 字段对齐,多 updated_at 判新)。
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct RawIssue {
    pub number: i64,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub state: String,
    pub labels: Vec<String>,
    pub comments_count: i64,
    pub updated_at: String,
}

/// raw 层的评论记录(id = GitHub 评论 id,去重键)。
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct RawComment {
    pub id: i64,
    pub issue_number: i64,
    pub author: String,
    pub body: String,
    pub created_at: String,
}

pub struct RawStore {
    dir: PathBuf,
}

fn repo_dir_name(repo: &str) -> String {
    repo.replace('/', "__")
}

impl RawStore {
    pub fn open(home: &std::path::Path, repo: &str) -> Result<Self> {
        let dir = home.join("raw").join(repo_dir_name(repo));
        std::fs::create_dir_all(&dir)
            .map_err(|e| Error::Io(std::io::Error::other(format!("raw dir: {e}"))))?;
        Ok(Self { dir })
    }

    fn issues_path(&self) -> PathBuf {
        self.dir.join("issues.jsonl.gz")
    }
    fn comments_path(&self) -> PathBuf {
        self.dir.join("comments.jsonl.gz")
    }

    /// 读行:优先 .gz;兼容旧裸 .jsonl(首次访问自动迁移为 .gz)。
    fn read_lines(path: &std::path::Path) -> Vec<String> {
        let plain = path.with_extension(""); // *.jsonl.gz → *.jsonl
        if !path.exists() && plain.exists() {
            if let Ok(text) = std::fs::read_to_string(&plain) {
                let _ = write_gzip(path, &text);
                let _ = std::fs::remove_file(&plain);
            }
        }
        read_gzip(path)
            .map(|s| {
                s.lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 合并一次抓取:issues 按 number upsert,comments 按 id 去重。
    pub fn merge(&self, issues: &[RawIssue], comments: &[RawComment]) -> Result<()> {
        // issues
        let mut map: HashMap<i64, RawIssue> = HashMap::new();
        for l in Self::read_lines(&self.issues_path()) {
            if let Ok(r) = serde_json::from_str::<RawIssue>(&l) {
                map.insert(r.number, r);
            }
        }
        for i in issues {
            let newer = match map.get(&i.number) {
                Some(old) => i.updated_at >= old.updated_at,
                None => true,
            };
            if newer {
                map.insert(i.number, i.clone());
            }
        }
        let mut lines: Vec<String> = map
            .into_values()
            .map(|r| serde_json::to_string(&r).unwrap_or_default())
            .collect();
        lines.sort_by_key(|_| 0); // 保持稳定:排序按 number
                                  // 重新按 number 排序输出(可 diff、可读)
        let mut sorted: Vec<(i64, String)> = lines
            .into_iter()
            .filter_map(|l| {
                serde_json::from_str::<RawIssue>(&l)
                    .ok()
                    .map(|r| (r.number, l))
            })
            .collect();
        sorted.sort_by_key(|(n, _)| *n);
        let text: String = sorted.into_iter().map(|(_, l)| format!("{l}\n")).collect();
        write_gzip(&self.issues_path(), &text)
            .map_err(|e| Error::Io(std::io::Error::other(format!("raw write: {e}"))))?;

        // comments
        let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
        let mut existing: Vec<String> = Self::read_lines(&self.comments_path());
        existing.retain(|l| {
            match serde_json::from_str::<RawComment>(l) {
                Ok(c) => seen.insert(c.id),
                Err(_) => true, // 解析失败的行保留(不静默丢数据)
            }
        });
        for c in comments {
            if seen.insert(c.id) {
                existing.push(serde_json::to_string(c).unwrap_or_default());
            }
        }
        existing.sort_by_key(|l| {
            serde_json::from_str::<RawComment>(l)
                .map(|c| (c.issue_number, c.id))
                .unwrap_or((0, 0))
        });
        let text: String = existing.into_iter().map(|l| format!("{l}\n")).collect();
        write_gzip(&self.comments_path(), &text)
            .map_err(|e| Error::Io(std::io::Error::other(format!("raw write: {e}"))))?;
        Ok(())
    }

    /// 读全量:issue 聚合其评论(时间序 = id 序近似;bot 过滤在嵌入层做)。
    pub fn load(&self, repo: &str) -> Result<Vec<IssueMeta>> {
        let mut issues: Vec<RawIssue> = Self::read_lines(&self.issues_path())
            .into_iter()
            .filter_map(|l| serde_json::from_str::<RawIssue>(&l).ok())
            .collect();
        issues.sort_by_key(|i| i.number);

        let mut by_issue: HashMap<i64, Vec<Comment>> = HashMap::new();
        for l in Self::read_lines(&self.comments_path()) {
            if let Ok(c) = serde_json::from_str::<RawComment>(&l) {
                by_issue.entry(c.issue_number).or_default().push(Comment {
                    author: c.author,
                    body: c.body,
                });
            }
        }
        Ok(issues
            .into_iter()
            .map(|i| IssueMeta {
                id: i.number, // raw 层以 number 为键;issues.id 自增与 GitHub 无关
                repo: repo.to_string(),
                number: i.number,
                kind: i.kind,
                title: i.title,
                body: i.body,
                state: i.state,
                labels: i.labels,
                comments_count: i.comments_count,
                comments: Some(by_issue.remove(&i.number).unwrap_or_default()),
                updated_at: i.updated_at,
            })
            .collect())
    }
}

/// gzip 全量写(JSONL 冷数据,压缩存储是默认)。
fn write_gzip(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    let f = std::fs::File::create(path)?;
    let mut enc = GzEncoder::new(f, Compression::default());
    enc.write_all(text.as_bytes())?;
    enc.finish()?;
    Ok(())
}

fn read_gzip(path: &std::path::Path) -> std::io::Result<String> {
    use flate2::read::GzDecoder;
    use std::io::Read;
    let f = std::fs::File::open(path)?;
    let mut dec = GzDecoder::new(f);
    let mut out = String::new();
    dec.read_to_string(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> RawStore {
        let dir = std::env::temp_dir().join(format!("gh-rag-raw-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        RawStore::open(&dir, "a/b").unwrap()
    }

    fn issue(n: i64, updated: &str, title: &str) -> RawIssue {
        RawIssue {
            number: n,
            kind: "issue".into(),
            title: title.into(),
            body: format!("body {n}"),
            state: "open".into(),
            labels: vec!["bug".into()],
            comments_count: 0,
            updated_at: updated.into(),
        }
    }

    fn comment(id: i64, n: i64, body: &str) -> RawComment {
        RawComment {
            id,
            issue_number: n,
            author: "alice".into(),
            body: body.into(),
            created_at: format!("2026-09-0{id}T00:00:00Z"),
        }
    }

    #[test]
    fn merge_upserts_by_number_and_dedups_comments() {
        let rs = tmp("merge");
        rs.merge(
            &[
                issue(1, "2026-09-01T00:00:00Z", "旧标题"),
                issue(2, "2026-09-01T00:00:00Z", "t2"),
            ],
            &[comment(10, 1, "c1")],
        )
        .unwrap();
        // 更新 1 号(新时间)→ 标题换;评论 10 不重复、11 追加
        rs.merge(
            &[issue(1, "2026-09-05T00:00:00Z", "新标题")],
            &[comment(10, 1, "c1"), comment(11, 1, "c2")],
        )
        .unwrap();
        let all = rs.load("a/b").unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].title, "新标题", "number upsert 取 updated_at 新者");
        let cs = all[0].comments.clone().unwrap();
        assert_eq!(cs.len(), 2, "评论按 id 去重");
        assert_eq!(cs[1].body, "c2");
    }

    #[test]
    fn stale_update_does_not_overwrite() {
        let rs = tmp("stale");
        rs.merge(&[issue(1, "2026-09-05T00:00:00Z", "新")], &[])
            .unwrap();
        rs.merge(&[issue(1, "2026-09-01T00:00:00Z", "旧")], &[])
            .unwrap();
        assert_eq!(rs.load("a/b").unwrap()[0].title, "新", "旧数据不覆盖新数据");
    }

    #[test]
    fn load_aggregates_comments_per_issue() {
        let rs = tmp("agg");
        rs.merge(
            &[issue(1, "t", "a"), issue(2, "t", "b")],
            &[
                comment(1, 1, "一"),
                comment(2, 2, "二"),
                comment(3, 1, "三"),
            ],
        )
        .unwrap();
        let all = rs.load("a/b").unwrap();
        assert_eq!(all[0].comments.as_ref().unwrap().len(), 2);
        assert_eq!(all[1].comments.as_ref().unwrap().len(), 1);
    }
}
