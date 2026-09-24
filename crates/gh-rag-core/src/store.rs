//! IssueStore:单文件 SQLite(metadata + BLOB 向量 + FTS5)。
//!
//! M1:读路径(检索)+ query_log 写入,库由 Python 版建立。
//! M2:补全 upsert 建库路径。
//! schema 与 Python 侧完全一致(普通表 + BLOB 列 + FTS5,跨语言设计)。

use crate::cjk::cjk_bigram;
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
    pub author: String,
    pub created_at: String,
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
        let store = Self { db };
        store.migrate_fts_cjk()?;
        Ok(store)
    }

    /// 幂等迁移:存量 FTS 索引是 unicode61 裸文本(整串 CJK 单 token),
    /// 与 bigram 索引不兼容 → manifest 无 'fts_cjk' 标记时 delete-all 后
    /// 从 issues 表(经 cjk_bigram)重建;空库直接写标记。
    fn migrate_fts_cjk(&self) -> Result<()> {
        if self.manifest_get("fts_cjk")?.is_some() {
            return Ok(());
        }
        let n: i64 = self
            .db
            .query_row("SELECT COUNT(*) FROM issues", [], |r| r.get(0))?;
        if n > 0 {
            self.db.execute(
                "INSERT INTO issues_fts(issues_fts) VALUES('delete-all')",
                [],
            )?;
            let mut stmt = self.db.prepare("SELECT id, title, body FROM issues")?;
            let rows: Vec<(i64, String, String)> = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<std::result::Result<_, _>>()?;
            drop(stmt);
            let tx = self.db.unchecked_transaction()?;
            {
                let mut ins =
                    tx.prepare("INSERT INTO issues_fts(rowid,title,body) VALUES(?1,?2,?3)")?;
                for (id, title, body) in &rows {
                    ins.execute(rusqlite::params![id, cjk_bigram(title), cjk_bigram(body)])?;
                }
            }
            tx.commit()?;
        }
        self.db.execute(
            "INSERT OR REPLACE INTO manifest(key,value) VALUES('fts_cjk','1')",
            [],
        )?;
        Ok(())
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
                // ESCAPE 转义 %/_/\:labels 含特殊字符(如 "c++100%")不再误匹配
                sql.push_str(" AND i.labels LIKE ? ESCAPE '\\'");
                args.push(Box::new(format!("%\"{}\"%", escape_like(lab))));
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
        labels: Option<&[String]>,
        limit: usize,
    ) -> Result<Vec<(i64, f32)>> {
        // CJK 双字组预处理(与索引侧同一变换),再按空白拆 token 逐个引号包裹
        let match_expr = cjk_bigram(phrase)
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
        if let Some(labels) = labels {
            for lab in labels {
                // 与 candidates 同一过滤面:labels 过滤两腿必须对齐
                sql.push_str(" AND i.labels LIKE ? ESCAPE '\\'");
                args.push(Box::new(format!("%\"{}\"%", escape_like(lab))));
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
            "SELECT id, repo, number, title, body, state, labels, comments_count, updated_at, kind, \
             COALESCE(author, ''), COALESCE(created_at, '') \
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
                    title: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    body: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    state: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    labels: parse_labels(r.get(6)?),
                    comments_count: r.get(7)?,
                    updated_at: r.get::<_, Option<String>>(8)?.unwrap_or_default(),
                    author: r.get(10)?,
                    created_at: r.get(11)?,
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
            "SELECT id, repo, number, title, body, state, labels, comments_count, updated_at, kind, \
             COALESCE(author, ''), COALESCE(created_at, '') \
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

    /// 标签侧面表:(repo, label, count),标签为 JSON 数组列,用 json_each 展开;
    /// 按仓库分组、仓内 count 降序。
    pub fn label_facets(&self) -> Result<Vec<(String, String, i64)>> {
        let mut stmt = self.db.prepare(
            "SELECT i.repo, je.value, COUNT(*) FROM issues i, json_each(i.labels) je \
             GROUP BY i.repo, je.value ORDER BY i.repo, COUNT(*) DESC",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 全量替换 (repo, number) 的正向关系(同事务 DELETE+INSERT,空切片 = 清空)。
    pub fn relations_replace(
        &self,
        repo: &str,
        number: i64,
        rels: &[crate::relations::Relation],
    ) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM relations WHERE repo = ? AND number = ?",
            rusqlite::params![repo, number],
        )?;
        for r in rels {
            tx.execute(
                "INSERT OR IGNORE INTO relations(repo, number, kind, target_repo, target_number)                  VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![repo, number, r.kind.as_str(), r.target_repo, r.target_number],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// 反向查询:所有指向 (repo, number) 的关系。fixes 的反向即 fixed_by,由调用方转义。
    pub fn relations_reverse(&self, repo: &str, number: i64) -> Result<Vec<(String, String, i64)>> {
        let mut stmt = self.db.prepare(
            "SELECT kind, repo, number FROM relations WHERE target_repo = ? AND target_number = ?              ORDER BY repo, number",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![repo, number], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
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
        if fingerprint_space(&cur) != fingerprint_space(&fp.0) {
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
                "INSERT INTO issues(id,repo,kind,number,title,body,state,labels,comments_count,author,created_at,updated_at,embedded_hash)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
                 ON CONFLICT(repo,number) DO UPDATE SET
                   kind=excluded.kind, title=excluded.title, body=excluded.body, state=excluded.state,
                   labels=excluded.labels, comments_count=excluded.comments_count,
                   author=excluded.author, created_at=excluded.created_at,
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
                        // 删除必须与插入同一变换(bigram 后的旧内容)
                        fts_del.execute(rusqlite::params![
                            old_id,
                            cjk_bigram(old_title),
                            cjk_bigram(old_body)
                        ])?;
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
                    it.meta.author,
                    it.meta.created_at,
                    it.meta.updated_at,
                    it.text_hash,
                ])?;
                let real_id = if row_id == 0 {
                    tx.last_insert_rowid()
                } else {
                    row_id
                };
                fts_ins.execute(rusqlite::params![
                    real_id,
                    cjk_bigram(&it.meta.title),
                    cjk_bigram(&it.meta.body)
                ])?;
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
                comments_count=?5, author=?6, created_at=?7, updated_at=?8
                WHERE repo=?9 AND number=?10",
            rusqlite::params![
                m.title,
                m.body,
                m.state,
                labels,
                m.comments_count,
                m.author,
                m.created_at,
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

    pub fn log_query(
        &self,
        tool: &str,
        query: &str,
        filters: Option<serde_json::Value>,
        results: &[(String, i64)],
    ) -> Result<()> {
        let results_json = serde_json::to_string(
            &results
                .iter()
                .map(|(r, n)| format!("{r}#{n}"))
                .collect::<Vec<_>>(),
        )
        .unwrap_or_default();
        self.db.execute(
            "INSERT INTO query_log(tool, query, filters, results) VALUES (?, ?, ?, ?)",
            rusqlite::params![tool, query, filters.map(|f| f.to_string()), results_json],
        )?;
        Ok(())
    }

    /// 标记 follow_up:目标已知(repo+number),落在**结果里包含该目标**的最近一次检索上;
    /// 没有任何检索结果包含该目标时不动任何行(返回 false)。
    pub fn mark_follow_up(&self, repo: &str, number: i64) -> Result<bool> {
        let target = format!("{repo}#{number}");
        let n = self.db.execute(
            "UPDATE query_log SET follow_up = ?2 WHERE id = \
             (SELECT id FROM query_log WHERE tool = 'search_issues' AND results LIKE ?1 \
              ORDER BY id DESC LIMIT 1)",
            rusqlite::params![format!("%\"{target}\"%"), target],
        )?;
        Ok(n > 0)
    }
}

/// `gh-rag report` 的统计结果(只读,近 N 天)。
#[derive(Debug, Default, PartialEq)]
pub struct QueryReport {
    pub days: u32,
    /// 查询总次数
    pub total: i64,
    /// 去重后的不同查询数
    pub unique: i64,
    /// follow_up 标记数 / 查询总次数(total=0 时为 0.0)
    pub follow_up_rate: f64,
    /// 高频查询 top 10:(query, 次数),次数降序
    pub top_queries: Vec<(String, i64)>,
    /// 按工具分布:(tool, 次数),次数降序
    pub by_tool: Vec<(String, i64)>,
}

impl IssueStore {
    /// 质量报表:近 `days` 天的 query_log 统计。只读,不动任何写入路径。
    pub fn query_report(&self, days: u32) -> Result<QueryReport> {
        let since = format!("datetime('now', '-{days} days')");
        let one = |sql: &str| -> Result<i64> {
            self.db
                .query_row(sql, [], |r| r.get(0))
                .map_err(|e| crate::Error::Io(std::io::Error::other(e.to_string())))
        };
        let total = one(&format!(
            "SELECT COUNT(*) FROM query_log WHERE ts >= {since}"
        ))?;
        let unique = one(&format!(
            "SELECT COUNT(DISTINCT query) FROM query_log WHERE ts >= {since}"
        ))?;
        let followed = one(&format!(
            "SELECT COUNT(*) FROM query_log WHERE ts >= {since} AND follow_up IS NOT NULL"
        ))?;
        let top_queries = self
            .db
            .prepare(&format!(
                "SELECT query, COUNT(*) c FROM query_log WHERE ts >= {since} \
                 GROUP BY query ORDER BY c DESC, query LIMIT 10"
            ))?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let by_tool = self
            .db
            .prepare(&format!(
                "SELECT COALESCE(tool, '-'), COUNT(*) c FROM query_log WHERE ts >= {since} \
                 GROUP BY tool ORDER BY c DESC"
            ))?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(QueryReport {
            days,
            total,
            unique,
            follow_up_rate: if total > 0 {
                followed as f64 / total as f64
            } else {
                0.0
            },
            top_queries,
            by_tool,
        })
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
        title: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
        body: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
        state: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
        labels: parse_labels(r.get(6)?),
        comments_count: r.get(7)?,
        updated_at: r.get::<_, Option<String>>(8)?.unwrap_or_default(),
        author: r.get::<_, Option<String>>(10)?.unwrap_or_default(),
        created_at: r.get::<_, Option<String>>(11)?.unwrap_or_default(),
        comments: None,
    })
}

fn parse_labels(raw: Option<String>) -> Vec<String> {
    raw.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 指纹的"向量空间"部分:`{model}|{impl}|len={n}|…` → (model, len)。
/// 公开给 bin 层(mcp 启动时做读路径指纹防线),impl 段不参与拦截判定
/// (黄金对齐守护同模型实现的互换性)。
pub fn fingerprint_space(fp: &str) -> (String, String) {
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

/// LIKE 通配符转义(配 `ESCAPE '\'`):`\` `%` `_` 前加反斜杠,
/// 防 labels/filter 值含 SQL LIKE 特殊字符造成误匹配。
pub(crate) fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\\' || c == '%' || c == '_' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(tag: &str) -> IssueStore {
        let dir = std::env::temp_dir().join(format!("gh-rag-store-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        IssueStore::create_fixture(&dir.join("t.sqlite")).unwrap()
    }

    fn log_at(s: &IssueStore, tool: &str, query: &str, ts_expr: &str, follow_up: Option<&str>) {
        s.db.execute(
            "INSERT INTO query_log(ts, tool, query, follow_up) \
                 VALUES (datetime('now', ?1), ?2, ?3, ?4)",
            rusqlite::params![ts_expr, tool, query, follow_up],
        )
        .unwrap();
    }

    #[test]
    fn label_facets_groups_and_orders_by_count() {
        let s = tmp_store("labfacets");
        // 两个仓:各多条标签、多计数
        let rows = [
            ("a/r", 1, r#"["bug","rust"]"#),
            ("a/r", 2, r#"["bug"]"#),
            ("a/r", 3, r#"["bug","help wanted","rust"]"#),
            ("b/r", 10, r#"["docs"]"#),
            ("b/r", 11, r#"["docs","ci"]"#),
        ];
        for (repo, id, labels) in rows {
            s.db.execute(
                "INSERT INTO issues(id, repo, number, labels) VALUES (?1,?2,?3,?4)",
                rusqlite::params![id, repo, id, labels],
            )
            .unwrap();
        }
        let got = s.label_facets().unwrap();
        assert_eq!(
            got,
            vec![
                ("a/r".into(), "bug".into(), 3),
                ("a/r".into(), "rust".into(), 2),
                ("a/r".into(), "help wanted".into(), 1),
                ("b/r".into(), "docs".into(), 2),
                ("b/r".into(), "ci".into(), 1),
            ]
        );
    }

    #[test]
    fn escape_like_quotes_wildcards() {
        assert_eq!(escape_like("c++100%"), "c++100\\%");
        assert_eq!(escape_like("a_b\\c"), "a\\_b\\\\c");
        assert_eq!(escape_like("普通"), "普通");
    }

    #[test]
    fn candidates_label_filter_escapes_wildcards() {
        let s = tmp_store("labesc");
        // 两条:label "100%bug" 与 "bug"(后者本不该被 %label% 命中……验证转义后互不串)
        for (id, labels) in [(1, r#"["100%bug"]"#), (2, r#"["axb"]"#), (3, r#"["a_b"]"#)] {
            s.db.execute(
                "INSERT INTO issues(id, repo, number, labels) VALUES (?1,'t/r',?1,?2)",
                rusqlite::params![id, labels],
            )
            .unwrap();
            s.db.execute(
                "INSERT INTO issues_vec(issue_id, embedding) VALUES (?1, x'00000000')",
                [id],
            )
            .unwrap();
        }
        // 精确查 "a_b":不该命中 "axb"(旧实现 _ 作通配符会误命中)
        let hit = s
            .candidates(None, None, Some(&["a_b".to_string()]))
            .unwrap();
        assert_eq!(hit.len(), 1, "a_b 不得匹配 axb");
        assert_eq!(hit[0].0, 3);
        // 精确查 "100%bug":% 作通配符时仍能命中,但须只此一条
        let hit = s
            .candidates(None, None, Some(&["100%bug".to_string()]))
            .unwrap();
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].0, 1);
        // 不存在的 label 不命中任何行
        assert!(s
            .candidates(None, None, Some(&["100xbug".to_string()]))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn log_query_persists_filters_json() {
        let s = tmp_store("logf");
        s.log_query(
            "search_issues",
            "q",
            Some(serde_json::json!({"state": "open"})),
            &[("t/r".into(), 1)],
        )
        .unwrap();
        let (f, r): (Option<String>, String) =
            s.db.query_row("SELECT filters, results FROM query_log", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(f.as_deref(), Some(r#"{"state":"open"}"#));
        assert_eq!(r, r#"["t/r#1"]"#);
    }

    #[test]
    fn mark_follow_up_targets_issue_not_latest_search() {
        let s = tmp_store("mfu");
        // 两次检索:第一次结果含 t/r#7,第二次不含
        s.log_query(
            "search_issues",
            "old query",
            None,
            &[("t/r".into(), 7), ("t/r".into(), 8)],
        )
        .unwrap();
        s.log_query("search_issues", "new query", None, &[("t/x".into(), 9)])
            .unwrap();
        assert!(s.mark_follow_up("t/r", 7).unwrap(), "命中旧检索行");
        let (qid, fu): (i64, Option<String>) =
            s.db.query_row(
                "SELECT id, follow_up FROM query_log WHERE follow_up IS NOT NULL",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(qid, 1, "标记落在结果含目标的检索行,而非最近一条");
        assert_eq!(fu.as_deref(), Some("t/r#7"));
        // 目标不在任何结果里 → 不动任何行
        assert!(!s.mark_follow_up("t/none", 1).unwrap());
    }

    #[test]
    fn mark_follow_up_does_not_match_repo_prefix() {
        let s = tmp_store("mfu2");
        // 结果里只有 cat/r#9:目标 at/r#9 不得被 LIKE 子串误标
        s.log_query("search_issues", "q", None, &[("cat/r".into(), 9)])
            .unwrap();
        assert!(
            !s.mark_follow_up("at/r", 9).unwrap(),
            "at/r#9 不得误标 cat/r#9"
        );
        // 精确目标仍能命中
        s.log_query("search_issues", "q2", None, &[("at/r".into(), 9)])
            .unwrap();
        assert!(s.mark_follow_up("at/r", 9).unwrap(), "精确目标应命中");
    }

    #[test]
    fn get_issue_tolerates_null_title_body() {
        let s = tmp_store("nullmeta");
        s.db.execute("INSERT INTO issues(repo, number) VALUES ('t/r', 1)", [])
            .unwrap();
        let m = s.get_issue("t/r", 1).unwrap().expect("行应存在");
        assert_eq!(m.title, "");
        assert_eq!(m.body, "");
    }

    #[test]
    fn relations_replace_and_reverse() {
        let s = tmp_store("relrev");
        use crate::relations::{parse_mentions, Relation, RelationKind};
        // 正向写入 + 重写覆盖(DELETE+INSERT 幂等)
        let rel = |k, tr: &str, n| Relation {
            kind: k,
            target_repo: tr.into(),
            target_number: n,
        };
        s.relations_replace(
            "t/r",
            7,
            &[
                rel(RelationKind::Fixes, "t/r", 3),
                rel(RelationKind::Refs, "o/x", 9),
            ],
        )
        .unwrap();
        let fwd = s.relations_of("t/r", 7).unwrap();
        assert_eq!(
            fwd,
            vec![
                ("fixes".into(), "t/r".into(), 3),
                ("refs".into(), "o/x".into(), 9)
            ]
        );
        // 反向:#3 被 #7 fixes
        let rev = s.relations_reverse("t/r", 3).unwrap();
        assert_eq!(rev, vec![("fixes".into(), "t/r".into(), 7)]);
        // 跨仓反向:o/x#9 被 t/r#7 refs
        let rev2 = s.relations_reverse("o/x", 9).unwrap();
        assert_eq!(rev2, vec![("refs".into(), "t/r".into(), 7)]);
        // 无记录 → 空数组
        assert!(s.relations_reverse("t/r", 99).unwrap().is_empty());
        // 重写同一 issue:旧记录全量替换,不残留
        s.relations_replace("t/r", 7, &[rel(RelationKind::Closes, "t/r", 4)])
            .unwrap();
        assert_eq!(
            s.relations_of("t/r", 7).unwrap(),
            vec![("closes".into(), "t/r".into(), 4)]
        );
        // 空切片 = 清空
        s.relations_replace("t/r", 7, &[]).unwrap();
        assert!(s.relations_of("t/r", 7).unwrap().is_empty());
        // parse_mentions 的产物可直接入库(联动冒烟)
        let m = parse_mentions("t/r", 8, "t", "fixes #5");
        s.relations_replace("t/r", 8, &m).unwrap();
        assert_eq!(
            s.relations_reverse("t/r", 5).unwrap(),
            vec![("fixes".into(), "t/r".into(), 8)]
        );
    }

    #[test]
    fn query_report_counts_window_and_dedup() {
        let s = tmp_store("report");
        // 窗口内(相对 now,离 1 天边界留余量):5 次查询,3 个去重,2 次同 query,1 次 follow_up
        log_at(&s, "search_issues", "a", "-2 day", None);
        log_at(&s, "search_issues", "a", "-3 day", None);
        log_at(&s, "search_issues", "b", "-4 day", Some("t/r#3"));
        log_at(&s, "get_issue_context", "-", "-5 day", None);
        // 窗口外:不得计入
        log_at(&s, "search_issues", "old", "-40 day", None);

        let r = s.query_report(7).unwrap();
        assert_eq!(r.total, 4);
        assert_eq!(r.unique, 3);
        assert!((r.follow_up_rate - 0.25).abs() < 1e-9);
        assert_eq!(r.top_queries[0], ("a".into(), 2));
        assert_eq!(r.by_tool.len(), 2);
        assert_eq!(r.by_tool[0], ("search_issues".into(), 3));
        // 空窗口:全零,除零安全
        let r0 = s.query_report(1).unwrap();
        assert_eq!(r0.total, 0);
        assert_eq!(r0.follow_up_rate, 0.0);
        assert!(r0.top_queries.is_empty());
    }
}
