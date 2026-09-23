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
    /// issue | pr(同一文档流,检索默认全收,返回体带 kind 供 agent 区分)
    pub kind: String,
    pub title: String,
    pub body: String,
    pub state: String,
    pub labels: Vec<String>,
    pub comments_count: i64,
    /// 评论内容(时间序);None = 从未拉取(存量兼容),Some = 已聚合
    pub comments: Option<Vec<crate::github::Comment>>,
    pub updated_at: String,
}

/// 单条写入载荷:meta + 向量(小端 f32)+ 内容 hash(增量跳过依据)。
pub struct UpsertItem {
    pub meta: IssueMeta,
    pub embedding: Vec<u8>,
    pub text_hash: String,
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
        // 幂等迁移:CREATE IF NOT EXISTS 补新表;ALTER 补新列(kind)
        db.execute_batch(SCHEMA)?;
        let _ = db.execute(
            "ALTER TABLE issues ADD COLUMN kind TEXT NOT NULL DEFAULT 'issue'",
            [],
        );
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
    /// phrase 按空白拆 token 逐个引号包裹(隐式 AND 语义;
    /// 防 `()` `"` 等 FTS 查询语法字符导致的 syntax error)。
    pub fn fts_search(
        &self,
        phrase: &str,
        repos: Option<&[String]>,
        state: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(i64, f32)>> {
        let match_expr = phrase
            .split_whitespace()
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" ");
        let match_expr = if match_expr.is_empty() {
            "\"\"".to_string()
        } else {
            match_expr
        };
        let mut sql = String::from(
            "SELECT f.rowid, bm25(issues_fts) FROM issues_fts f \
             JOIN issues i ON i.id = f.rowid WHERE issues_fts MATCH ?",
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(match_expr)];
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
            "SELECT id, repo, number, title, body, state, labels, comments_count, updated_at, kind \
             FROM issues WHERE id IN ({ph})"
        );
        let mut stmt = self.db.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(ids.iter()), |r| {
                Ok(IssueMeta {
                    comments: None,
                    id: r.get(0)?,
                    repo: r.get(1)?,
                    number: r.get(2)?,
                    kind: r
                        .get::<_, Option<String>>(9)?
                        .unwrap_or_else(|| "issue".into()),
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

    /// 读单条 issue 的评论(时间序 = 写入序)。
    fn comments_of(&self, issue_id: i64) -> Result<Vec<crate::github::Comment>> {
        let mut stmt = self
            .db
            .prepare("SELECT author, body FROM issue_comments WHERE issue_id=?1 ORDER BY idx")?;
        let rows = stmt
            .query_map([issue_id], |r| {
                Ok(crate::github::Comment {
                    author: r.get(0)?,
                    body: r.get(1)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_issue(&self, repo: &str, number: i64) -> Result<Option<IssueMeta>> {
        let mut stmt = self.db.prepare(
            "SELECT id, repo, number, title, body, state, labels, comments_count, updated_at, kind \
             FROM issues WHERE repo = ? AND number = ?",
        )?;
        let mut rows = stmt.query_map([repo, &number.to_string()], map_meta)?;
        match rows.next().transpose()? {
            Some(mut m) => {
                m.comments = Some(self.comments_of(m.id)?);
                Ok(Some(m))
            }
            None => Ok(None),
        }
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
        let v = self
            .db
            .query_row("SELECT value FROM manifest WHERE key=?1", [key], |r| {
                r.get(0)
            })
            .ok();
        Ok(v)
    }

    /// (embedded_hash, 是否已有向量);None = 行不存在。增量判定用。
    pub fn embedded_state(&self, repo: &str, number: i64) -> Result<Option<(String, bool)>> {
        let v: Option<(String, bool)> = self
            .db
            .query_row(
                "SELECT i.embedded_hash, EXISTS(SELECT 1 FROM issues_vec v WHERE v.issue_id = i.id)
                 FROM issues i WHERE i.repo=?1 AND i.number=?2",
                rusqlite::params![repo, number],
                |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        r.get(1)?,
                    ))
                },
            )
            .ok();
        Ok(v)
    }
    // -- 写路径(M2:建库/增量) ------------------------------------------

    /// 指纹钉死,分段比较:
    /// - 空间段(model|len)不同 → FingerprintMismatch;
    /// - 空间同、文本组装参数(tr/body/cq/cc)任一不同 → Config(重建索引);
    /// - 仅 impl 段不同 → 警告放行(黄金对齐守护互换性);
    /// - 旧格式(无 tr= 段)→ 放行并覆写新指纹(存量库自动迁移,零重嵌)。
    pub fn ensure_embedding_fp(&self, fp: &crate::embedder::EmbeddingFingerprint) -> Result<()> {
        let existing = self.manifest_get("embedding_fp")?;
        let cur = match existing {
            None => {
                self.db.execute(
                    "INSERT OR REPLACE INTO manifest(key,value) VALUES('embedding_fp',?1)",
                    [&fp.0],
                )?;
                return Ok(());
            }
            Some(cur) => cur,
        };
        if space_of(&cur) != space_of(&fp.0) {
            return Err(Error::FingerprintMismatch {
                db: cur,
                current: fp.0.clone(),
            });
        }
        // 旧格式(无组装参数段):hash 规则未变,放行并迁移
        if seg_of(&cur, "tr=").is_empty() {
            self.db.execute(
                "INSERT OR REPLACE INTO manifest(key,value) VALUES('embedding_fp',?1)",
                [&fp.0],
            )?;
            return Ok(());
        }
        let old_asm = (
            seg_of(&cur, "tr="),
            seg_of(&cur, "body="),
            seg_of(&cur, "cq="),
            seg_of(&cur, "cc="),
        );
        let new_asm = (
            seg_of(&fp.0, "tr="),
            seg_of(&fp.0, "body="),
            seg_of(&fp.0, "cq="),
            seg_of(&fp.0, "cc="),
        );
        if old_asm != new_asm {
            return Err(Error::Config(
                "文本组装参数变更,存量向量不兼容,请重建索引(rm index.sqlite 后 sync)".into(),
            ));
        }
        if cur != fp.0 {
            eprintln!(
                "[gh-rag] embedding impl 变更({cur} -> {}):同空间,继续",
                fp.0
            );
        }
        Ok(())
    }

    /// 批量 upsert:单事务。幂等;id 稳定(已有行保留原 id,向量/FTS 同步替换)。
    pub fn upsert_batch(&self, items: &[UpsertItem]) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        {
            let mut sel =
                tx.prepare("SELECT id,title,body FROM issues WHERE repo=?1 AND number=?2")?;
            let mut ins = tx.prepare(
                "INSERT INTO issues(id,repo,kind,number,title,body,state,labels,comments_count,updated_at,embedded_hash)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
                 ON CONFLICT(repo,number) DO UPDATE SET
                   kind=excluded.kind, title=excluded.title, body=excluded.body, state=excluded.state,
                   labels=excluded.labels, comments_count=excluded.comments_count,
                   updated_at=excluded.updated_at, embedded_hash=excluded.embedded_hash")?;
            let mut fts_ins =
                tx.prepare("INSERT INTO issues_fts(rowid,title,body) VALUES(?1,?2,?3)")?;
            let mut fts_del = tx.prepare(
                "INSERT INTO issues_fts(issues_fts,rowid,title,body) VALUES('delete',?1,?2,?3)",
            )?;
            let mut vec_put =
                tx.prepare("INSERT OR REPLACE INTO issues_vec(issue_id,embedding) VALUES(?1,?2)")?;
            for it in items {
                let labels =
                    serde_json::to_string(&it.meta.labels).unwrap_or_else(|_| "[]".to_string());
                // 已有行:保 id;contentless FTS 更新 = delete(旧值) + insert(新值)
                let existing: Option<(i64, String, String)> = sel
                    .query_row([&it.meta.repo, &it.meta.number.to_string()], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })
                    .ok();
                let row_id = match &existing {
                    Some((old_id, old_title, old_body)) => {
                        fts_del.execute(rusqlite::params![old_id, old_title, old_body])?;
                        *old_id
                    }
                    None => 0i64, // 0 = 自增
                };
                ins.execute(rusqlite::params![
                    if row_id == 0 { None } else { Some(row_id) },
                    it.meta.repo,
                    it.meta.kind,
                    it.meta.number,
                    it.meta.title,
                    it.meta.body,
                    it.meta.state,
                    labels,
                    it.meta.comments_count,
                    it.meta.updated_at,
                    it.text_hash,
                ])?;
                let real_id = if row_id == 0 {
                    tx.last_insert_rowid()
                } else {
                    row_id
                };
                fts_ins.execute(rusqlite::params![real_id, it.meta.title, it.meta.body])?;
                vec_put.execute(rusqlite::params![real_id, it.embedding])?;
                // 评论整组替换(Some = 本批已聚合;None = 保持存量不动)
                if let Some(cs) = &it.meta.comments {
                    tx.execute("DELETE FROM issue_comments WHERE issue_id=?1", [real_id])?;
                    for (idx, c) in cs.iter().enumerate() {
                        tx.execute(
                            "INSERT OR REPLACE INTO issue_comments(issue_id,idx,author,body) VALUES(?1,?2,?3,?4)",
                            rusqlite::params![real_id, idx as i64, c.author, c.body],
                        )?;
                    }
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// 仅更新元数据与评论(hash 未变、向量保留):嵌入跳过但讨论内容要落库。
    pub fn upsert_meta_and_comments(&self, m: &IssueMeta) -> Result<()> {
        let labels = serde_json::to_string(&m.labels).unwrap_or_else(|_| "[]".to_string());
        self.db.execute(
            "UPDATE issues SET title=?1, body=?2, state=?3, labels=?4,
                comments_count=?5, updated_at=?6 WHERE repo=?7 AND number=?8",
            rusqlite::params![
                m.title,
                m.body,
                m.state,
                labels,
                m.comments_count,
                m.updated_at,
                m.repo,
                m.number
            ],
        )?;
        let row_id: Option<i64> = self
            .db
            .query_row(
                "SELECT id FROM issues WHERE repo=?1 AND number=?2",
                rusqlite::params![m.repo, m.number],
                |r| r.get(0),
            )
            .ok();
        if let Some(id) = row_id {
            if let Some(cs) = &m.comments {
                self.db
                    .execute("DELETE FROM issue_comments WHERE issue_id=?1", [id])?;
                for (idx, c) in cs.iter().enumerate() {
                    self.db.execute(
                        "INSERT OR REPLACE INTO issue_comments(issue_id,idx,author,body) VALUES(?1,?2,?3,?4)",
                        rusqlite::params![id, idx as i64, c.author, c.body],
                    )?;
                }
            }
        }
        Ok(())
    }

    /// 增量游标:该仓库上次同步看到的最新 updated_at(ISO8601,含 Z)。
    pub fn sync_cursor(&self, repo: &str) -> Result<Option<String>> {
        let v: Option<String> = self
            .db
            .query_row(
                "SELECT cursor_updated_at FROM sync_state WHERE repo=?1",
                [repo],
                |r| r.get(0),
            )
            .ok();
        Ok(v)
    }

    /// 推进游标(记录本次同步时间)。
    pub fn sync_advance(&self, repo: &str, cursor: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO sync_state(repo,cursor_updated_at,last_sync_at)
             VALUES(?1,?2,datetime('now'))
             ON CONFLICT(repo) DO UPDATE SET cursor_updated_at=excluded.cursor_updated_at, last_sync_at=excluded.last_sync_at",
            rusqlite::params![repo, cursor],
        )?;
        Ok(())
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
        kind: r
            .get::<_, Option<String>>(9)?
            .unwrap_or_else(|| "issue".into()),
        title: r.get(3)?,
        body: r.get(4)?,
        state: r.get(5)?,
        labels: parse_labels(r.get(6)?),
        comments_count: r.get(7)?,
        updated_at: r.get(8)?,
        comments: None,
    })
}

fn parse_labels(raw: Option<String>) -> Vec<String> {
    raw.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 指纹的"向量空间"部分:`{model}|{impl}|len={n}|…` → (model, len)。
/// impl 段不参与拦截判定(黄金对齐守护同模型实现的互换性)。
fn space_of(fp: &str) -> (String, String) {
    let model = fp.split('|').next().unwrap_or("").to_string();
    (model, seg_of(fp, "len="))
}

/// 取 `key` 段的值(段形如 `{key}{value}`,value 到下一个 '|' 为止);无则空串。
fn seg_of(fp: &str, key: &str) -> String {
    fp.split('|')
        .find_map(|s| s.strip_prefix(key))
        .unwrap_or("")
        .to_string()
}

pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS issues(
  id INTEGER PRIMARY KEY, repo TEXT NOT NULL, number INTEGER NOT NULL,
  kind TEXT NOT NULL DEFAULT 'issue',
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
CREATE TABLE IF NOT EXISTS issue_comments(
  issue_id INTEGER NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
  idx INTEGER NOT NULL, author TEXT, body TEXT,
  PRIMARY KEY(issue_id, idx));
CREATE TABLE IF NOT EXISTS sync_state(
  repo TEXT PRIMARY KEY, cursor_updated_at TEXT, last_sync_at TEXT);
CREATE TABLE IF NOT EXISTS manifest(key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE IF NOT EXISTS query_log(
  id INTEGER PRIMARY KEY, ts TEXT DEFAULT (datetime('now')),
  tool TEXT, query TEXT, filters TEXT, results TEXT, follow_up TEXT);
"#;
