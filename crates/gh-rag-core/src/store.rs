//! IssueStore:单文件 SQLite(metadata + BLOB 向量 + FTS5)。
//!
//! M1:读路径(检索)+ query_log 写入,库由 Python 版建立。
//! M2:补全 upsert 建库路径。
//! schema 与 Python 侧完全一致(普通表 + BLOB 列 + FTS5,跨语言设计)。

use crate::{Error, Result};
use rusqlite::Connection;
use std::path::Path;

pub struct IssueStore {
    pub db: Connection,
}

#[derive(Debug, Clone)]
pub struct IssueMeta {
    pub id: i64,
    pub repo: String,
    pub number: i64,
    pub title: String,
    pub body: String,
    pub state: String,
    pub labels: Vec<String>,
    pub comments_count: i64,
    pub updated_at: String,
}

impl IssueStore {
    pub fn new(db_path: &Path) -> Result<Self> {
        if !db_path.exists() {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("index not found at {} — run sync first", db_path.display()),
            )));
        }
        let db = Connection::open(db_path)?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        Ok(Self { db })
    }

    /// 建最小 fixture 库(测试用;M2 由完整 upsert 取代)。
    pub fn create_fixture(db_path: &Path) -> Result<Self> {
        let db = Connection::open(db_path)?;
        db.execute_batch(SCHEMA)?;
        Ok(Self { db })
    }

    // -- 读路径 ----------------------------------------------------------

    /// 过滤后的 (issue_id, embedding blob) 对,供暴力扫描。
    pub fn candidates(
        &self,
        repos: Option<&[String]>,
        state: Option<&str>,
        labels: Option<&[String]>,
    ) -> Result<Vec<(i64, Vec<u8>)>> {
        let mut sql = String::from(
            "SELECT i.id, v.embedding FROM issues i JOIN issues_vec v ON v.issue_id = i.id WHERE 1=1",
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(repos) = repos {
            sql.push_str(&format!(
                " AND i.repo IN ({})",
                (0..repos.len()).map(|_| "?").collect::<Vec<_>>().join(",")
            ));
            for r in repos {
                args.push(Box::new(r.clone()));
            }
        }
        if let Some(state) = state {
            if state != "all" {
                sql.push_str(" AND i.state = ?");
                args.push(Box::new(state.to_string()));
            }
        }
        if let Some(labels) = labels {
            for lab in labels {
                sql.push_str(" AND i.labels LIKE ?");
                args.push(Box::new(format!("%\"{lab}\"%")));
            }
        }
        let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let mut stmt = self.db.prepare(&sql)?;
        let rows = stmt
            .query_map(refs.as_slice(), |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// FTS5(BM25)召回,rank 升序(越小越相关)。
    pub fn fts_search(
        &self,
        phrase: &str,
        repos: Option<&[String]>,
        state: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(i64, f32)>> {
        let mut sql = String::from(
            "SELECT f.rowid, bm25(issues_fts) FROM issues_fts f \
             JOIN issues i ON i.id = f.rowid WHERE issues_fts MATCH ?",
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(phrase.to_string())];
        if let Some(repos) = repos {
            sql.push_str(&format!(
                " AND i.repo IN ({})",
                (0..repos.len()).map(|_| "?").collect::<Vec<_>>().join(",")
            ));
            for r in repos {
                args.push(Box::new(r.clone()));
            }
        }
        if let Some(state) = state {
            if state != "all" {
                sql.push_str(" AND i.state = ?");
                args.push(Box::new(state.to_string()));
            }
        }
        sql.push_str(" ORDER BY 2 LIMIT ?");
        args.push(Box::new(limit as i64));
        let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let mut stmt = self.db.prepare(&sql)?;
        let rows = stmt
            .query_map(refs.as_slice(), |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, f32>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn meta(&self, ids: &[i64]) -> Result<Vec<IssueMeta>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let ph = (0..ids.len()).map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, repo, number, title, body, state, labels, comments_count, updated_at \
             FROM issues WHERE id IN ({ph})"
        );
        let mut stmt = self.db.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(ids.iter()), |r| {
                Ok(IssueMeta {
                    id: r.get(0)?,
                    repo: r.get(1)?,
                    number: r.get(2)?,
                    title: r.get(3)?,
                    body: r.get(4)?,
                    state: r.get(5)?,
                    labels: parse_labels(r.get(6)?),
                    comments_count: r.get(7)?,
                    updated_at: r.get(8)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_issue(&self, repo: &str, number: i64) -> Result<Option<IssueMeta>> {
        let mut stmt = self.db.prepare(
            "SELECT id, repo, number, title, body, state, labels, comments_count, updated_at \
             FROM issues WHERE repo = ? AND number = ?",
        )?;
        let mut rows = stmt.query_map([repo, &number.to_string()], map_meta)?;
        Ok(rows.next().transpose()?)
    }

    pub fn get_embedding_blob(&self, issue_id: i64) -> Result<Option<Vec<u8>>> {
        let mut stmt = self
            .db
            .prepare("SELECT embedding FROM issues_vec WHERE issue_id = ?")?;
        let mut rows = stmt.query_map([issue_id], |r| r.get::<_, Vec<u8>>(0))?;
        Ok(rows.next().transpose()?)
    }

    pub fn repo_stats(&self) -> Result<Vec<(String, i64, String)>> {
        let mut stmt = self.db.prepare(
            "SELECT i.repo, COUNT(*), COALESCE(MAX(s.last_sync_at), '-') FROM issues i \
             LEFT JOIN sync_state s ON s.repo = i.repo GROUP BY i.repo ORDER BY i.repo",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn relations_of(&self, repo: &str, number: i64) -> Result<Vec<(String, String, i64)>> {
        let mut stmt = self.db.prepare(
            "SELECT kind, target_repo, target_number FROM relations WHERE repo = ? AND number = ?",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![repo, number], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn manifest_get(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self
            .db
            .prepare("SELECT value FROM manifest WHERE key = ?")?;
        let mut rows = stmt.query_map([key], |r| r.get::<_, String>(0))?;
        Ok(rows.next().transpose()?)
    }

    // -- query_log(检索质量飞轮的落地) -----------------------------------

    pub fn log_query(&self, tool: &str, query: &str, results: &[(String, i64)]) -> Result<()> {
        let results_json = serde_json::to_string(
            &results
                .iter()
                .map(|(r, n)| format!("{r}#{n}"))
                .collect::<Vec<_>>(),
        )
        .unwrap_or_default();
        self.db.execute(
            "INSERT INTO query_log(tool, query, results) VALUES (?, ?, ?)",
            rusqlite::params![tool, query, results_json],
        )?;
        Ok(())
    }

    pub fn mark_follow_up(&self, _ref: &str) -> Result<()> {
        self.db.execute(
            "UPDATE query_log SET follow_up = ? WHERE id = \
             (SELECT id FROM query_log WHERE tool = 'search_issues' ORDER BY id DESC LIMIT 1)",
            rusqlite::params![_ref],
        )?;
        Ok(())
    }
}

fn map_meta(r: &rusqlite::Row<'_>) -> rusqlite::Result<IssueMeta> {
    Ok(IssueMeta {
        id: r.get(0)?,
        repo: r.get(1)?,
        number: r.get(2)?,
        title: r.get(3)?,
        body: r.get(4)?,
        state: r.get(5)?,
        labels: parse_labels(r.get(6)?),
        comments_count: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

fn parse_labels(raw: Option<String>) -> Vec<String> {
    raw.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS issues(
  id INTEGER PRIMARY KEY, repo TEXT NOT NULL, number INTEGER NOT NULL,
  title TEXT, body TEXT, state TEXT, labels TEXT,
  author TEXT, comments_count INTEGER DEFAULT 0,
  created_at TEXT, updated_at TEXT, embedded_hash TEXT,
  UNIQUE(repo, number));
CREATE TABLE IF NOT EXISTS issues_vec(
  issue_id INTEGER PRIMARY KEY REFERENCES issues(id) ON DELETE CASCADE,
  embedding BLOB NOT NULL);
CREATE VIRTUAL TABLE IF NOT EXISTS issues_fts USING fts5(title, body, content='');
CREATE TABLE IF NOT EXISTS relations(
  repo TEXT, number INTEGER, kind TEXT, target_repo TEXT, target_number INTEGER,
  PRIMARY KEY(repo, number, kind, target_repo, target_number));
CREATE TABLE IF NOT EXISTS sync_state(
  repo TEXT PRIMARY KEY, cursor_updated_at TEXT, last_sync_at TEXT);
CREATE TABLE IF NOT EXISTS manifest(key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE IF NOT EXISTS query_log(
  id INTEGER PRIMARY KEY, ts TEXT DEFAULT (datetime('now')),
  tool TEXT, query TEXT, filters TEXT, results TEXT, follow_up TEXT);
"#;
