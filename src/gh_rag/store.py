"""IssueStore: single-file SQLite (metadata + BLOB vectors + FTS5)."""
from __future__ import annotations

import json
import sqlite3
from pathlib import Path

SCHEMA_VERSION = "1"

_DDL = [
    """CREATE TABLE IF NOT EXISTS issues(
      id INTEGER PRIMARY KEY,
      repo TEXT NOT NULL, number INTEGER NOT NULL,
      title TEXT, body TEXT, state TEXT, labels TEXT,
      author TEXT, comments_count INTEGER DEFAULT 0,
      created_at TEXT, updated_at TEXT,
      embedded_hash TEXT,
      UNIQUE(repo, number))""",
    """CREATE TABLE IF NOT EXISTS issues_vec(
      issue_id INTEGER PRIMARY KEY REFERENCES issues(id) ON DELETE CASCADE,
      embedding BLOB NOT NULL)""",
    """CREATE VIRTUAL TABLE IF NOT EXISTS issues_fts USING fts5(
      title, body, content='')""",
    """CREATE TABLE IF NOT EXISTS relations(
      repo TEXT, number INTEGER, kind TEXT,
      target_repo TEXT, target_number INTEGER,
      PRIMARY KEY(repo, number, kind, target_repo, target_number))""",
    """CREATE TABLE IF NOT EXISTS sync_state(
      repo TEXT PRIMARY KEY, cursor_updated_at TEXT, last_sync_at TEXT)""",
    """CREATE TABLE IF NOT EXISTS manifest(
      key TEXT PRIMARY KEY, value TEXT)""",
    """CREATE TABLE IF NOT EXISTS query_log(
      id INTEGER PRIMARY KEY, ts TEXT DEFAULT (datetime('now')),
      tool TEXT, query TEXT, filters TEXT, results TEXT, follow_up TEXT)""",
    "CREATE INDEX IF NOT EXISTS idx_issues_repo_state ON issues(repo, state)",
]


class IssueStore:
    def __init__(self, db_path: Path):
        db_path.parent.mkdir(parents=True, exist_ok=True)
        self.db = sqlite3.connect(db_path, check_same_thread=False)
        self.db.execute("PRAGMA journal_mode=WAL")
        for stmt in _DDL:
            self.db.execute(stmt)
        self.db.commit()

    def close(self):
        self.db.close()

    # -- manifest ----------------------------------------------------------

    def set_manifest(self, key: str, value: str):
        self.db.execute(
            "INSERT INTO manifest(key,value) VALUES(?,?) "
            "ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            (key, value),
        )
        self.db.commit()

    def get_manifest(self, key: str) -> str | None:
        row = self.db.execute("SELECT value FROM manifest WHERE key=?", (key,)).fetchone()
        return row[0] if row else None

    def ensure_embedding_fp(self, fp: str):
        """Discipline: vector space must match the pinned environment."""
        cur = self.get_manifest("embedding_fp")
        if cur is None:
            self.set_manifest("embedding_fp", fp)
            self.set_manifest("schema_version", SCHEMA_VERSION)
        elif cur != fp:
            raise RuntimeError(
                f"embedding fingerprint mismatch: db has '{cur}', current '{fp}'. "
                "Run `gh-rag rebuild` then re-sync."
            )

    # -- sync state --------------------------------------------------------

    def get_cursor(self, repo: str) -> str | None:
        row = self.db.execute(
            "SELECT cursor_updated_at FROM sync_state WHERE repo=?", (repo,)
        ).fetchone()
        return row[0] if row else None

    def set_cursor(self, repo: str, cursor: str):
        self.db.execute(
            "INSERT INTO sync_state(repo,cursor_updated_at,last_sync_at) VALUES(?,?,datetime('now')) "
            "ON CONFLICT(repo) DO UPDATE SET cursor_updated_at=excluded.cursor_updated_at,"
            "last_sync_at=datetime('now')",
            (repo, cursor),
        )
        self.db.commit()

    # -- upsert ------------------------------------------------------------

    def existing_hash(self, repo: str, number: int) -> str | None:
        row = self.db.execute(
            "SELECT embedded_hash FROM issues WHERE repo=? AND number=?", (repo, number)
        ).fetchone()
        return row[0] if row else None

    def upsert_issue(self, item: dict, embedding: bytes, embedded_hash: str):
        with self.db:
            row = self.db.execute(
                "SELECT id FROM issues WHERE repo=? AND number=?",
                (item["repo"], item["number"]),
            ).fetchone()
            if row:
                iid = row[0]
                self.db.execute(
                    """UPDATE issues SET title=?,body=?,state=?,labels=?,author=?,
                       comments_count=?,created_at=?,updated_at=?,embedded_hash=?
                       WHERE id=?""",
                    (
                        item["title"], item["body"], item["state"],
                        json.dumps(item["labels"], ensure_ascii=False),
                        item["author"], item["comments_count"],
                        item["created_at"], item["updated_at"], embedded_hash, iid,
                    ),
                )
            else:
                cur = self.db.execute(
                    """INSERT INTO issues(repo,number,title,body,state,labels,author,
                       comments_count,created_at,updated_at,embedded_hash)
                       VALUES(?,?,?,?,?,?,?,?,?,?,?)""",
                    (
                        item["repo"], item["number"], item["title"], item["body"],
                        item["state"], json.dumps(item["labels"], ensure_ascii=False),
                        item["author"], item["comments_count"],
                        item["created_at"], item["updated_at"], embedded_hash,
                    ),
                )
                iid = cur.lastrowid
            # vec0 无原地更新语义:BLOB 方案下同样用 delete+insert 保持简单一致
            self.db.execute("DELETE FROM issues_vec WHERE issue_id=?", (iid,))
            self.db.execute(
                "INSERT INTO issues_vec(issue_id,embedding) VALUES(?,?)", (iid, embedding)
            )
            self.db.execute("DELETE FROM issues_fts WHERE rowid=?", (iid,))
            self.db.execute(
                "INSERT INTO issues_fts(rowid,title,body) VALUES(?,?,?)",
                (iid, item["title"], item["body"]),
            )

    # -- retrieval primitives ---------------------------------------------

    def candidates(
        self,
        repos: list[str] | None = None,
        state: str | None = None,
        labels: list[str] | None = None,
        limit: int | None = None,
    ) -> list[tuple[int, bytes]]:
        """Filtered (id, embedding-blob) pairs for brute-force scan."""
        sql = (
            "SELECT i.id, v.embedding FROM issues i JOIN issues_vec v ON v.issue_id=i.id WHERE 1=1"
        )
        args: list = []
        if repos:
            sql += f" AND i.repo IN ({','.join('?' * len(repos))})"
            args.extend(repos)
        if state and state != "all":
            sql += " AND i.state=?"
            args.append(state)
        for lab in labels or []:
            sql += " AND i.labels LIKE ?"
            args.append(f'%"{lab}"%')
        if limit:
            sql += " LIMIT ?"
            args.append(limit)
        return self.db.execute(sql, args).fetchall()

    def fts_search(
        self,
        match: str,
        repos: list[str] | None = None,
        state: str | None = None,
        limit: int = 30,
    ) -> list[tuple[int, float]]:
        sql = (
            "SELECT f.rowid, bm25(issues_fts) AS score FROM issues_fts f "
            "JOIN issues i ON i.id=f.rowid WHERE issues_fts MATCH ?"
        )
        args: list = [match]
        if repos:
            sql += f" AND i.repo IN ({','.join('?' * len(repos))})"
            args.extend(repos)
        if state and state != "all":
            sql += " AND i.state=?"
            args.append(state)
        sql += " ORDER BY score LIMIT ?"
        args.append(limit)
        return self.db.execute(sql, args).fetchall()

    def meta(self, ids: list[int]) -> dict[int, dict]:
        if not ids:
            return {}
        ph = ",".join("?" * len(ids))
        rows = self.db.execute(
            f"SELECT id,repo,number,title,body,state,labels,comments_count,updated_at "
            f"FROM issues WHERE id IN ({ph})",
            ids,
        ).fetchall()
        return {
            r[0]: {
                "repo": r[1], "number": r[2], "title": r[3], "body": r[4],
                "state": r[5], "labels": json.loads(r[6] or "[]"),
                "comments_count": r[7], "updated_at": r[8],
            }
            for r in rows
        }

    def get_issue(self, repo: str, number: int) -> dict | None:
        row = self.db.execute(
            "SELECT id FROM issues WHERE repo=? AND number=?", (repo, number)
        ).fetchone()
        if not row:
            return None
        m = self.meta([row[0]])
        d = dict(m[row[0]])
        d["id"] = row[0]
        return d

    def get_embedding(self, issue_id: int) -> bytes | None:
        row = self.db.execute(
            "SELECT embedding FROM issues_vec WHERE issue_id=?", (issue_id,)
        ).fetchone()
        return row[0] if row else None

    def relations_of(self, repo: str, number: int) -> list[dict]:
        rows = self.db.execute(
            "SELECT kind,target_repo,target_number FROM relations WHERE repo=? AND number=?",
            (repo, number),
        ).fetchall()
        return [
            {"kind": r[0], "repo": r[1], "number": r[2]} for r in rows
        ]

    def repo_stats(self) -> list[dict]:
        rows = self.db.execute(
            "SELECT repo, COUNT(*), MAX(updated_at) FROM issues GROUP BY repo ORDER BY repo"
        ).fetchall()
        sync = {
            r[0]: (r[1], r[2])
            for r in self.db.execute(
                "SELECT repo,cursor_updated_at,last_sync_at FROM sync_state"
            ).fetchall()
        }
        return [
            {
                "repo": r[0], "issues": r[1],
                "latest_issue_at": r[2],
                "last_sync_at": sync.get(r[0], ("", ""))[1],
            }
            for r in rows
        ]

    # -- query log ---------------------------------------------------------

    def append_query_log(self, tool: str, query: str, filters: dict, results: list):
        with self.db:
            self.db.execute(
                "INSERT INTO query_log(tool,query,filters,results) VALUES(?,?,?,?)",
                (tool, query, json.dumps(filters, ensure_ascii=False),
                 json.dumps(results, ensure_ascii=False)),
            )

    def mark_follow_up(self, ref: str):
        """Record that the most recent search led to a full-context fetch."""
        with self.db:
            self.db.execute(
                "UPDATE query_log SET follow_up=? WHERE id="
                "(SELECT id FROM query_log WHERE tool='search_issues' ORDER BY id DESC LIMIT 1)",
                (ref,),
            )
