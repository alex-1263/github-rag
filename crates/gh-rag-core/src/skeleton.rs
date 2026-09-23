//! 骨架库(分发形态):向量 + 元数据 + 嵌入哈希,不含正文/评论全文。
//!
//! 版权模型(issue/评论文字版权归各作者,GitHub ToS D.5 仅授权 fork 复制):
//! 分发物只含**衍生数据**(向量)与**事实元数据**(repo/number/kind/state/时间/hash);
//! 全文由每个用户本地 `sync` 补齐(走 API = 正当阅读),hash 对齐使向量零重嵌。
//!
//! 安全模型(外来 SQLite 即潜在恶意输入):
//! - `import_skeleton` 只读打开源库,**不执行其中任何 DDL/SQL**;
//! - 逐列白名单拷贝进本地自建的表;任何多余表/触发器一概无视;
//! - 指纹必须与本地嵌入配置匹配,否则拒绝(防向量空间混用)。

use crate::store::{IssueStore, SCHEMA};
use crate::{Error, Result};
use rusqlite::Connection;

/// manifest 允许透传的键(其余键不拷)。
const MANIFEST_KEYS: &[&str] = &["embedding_fp", "schema_version"];

/// issues 允许拷贝的列(**无任何文本内容列**)。
const ISSUE_COLS: &str = "id, repo, kind, number, state, updated_at, embedded_hash";

/// 导入报告。
#[derive(Debug, Default, PartialEq)]
pub struct ImportReport {
    pub issues: usize,
    pub vectors: usize,
    pub fingerprint: Option<String>,
}

/// 从完整库导出骨架库(自身库 → 白名单列;含正文亦不带出)。
pub fn export_skeleton(src: &IssueStore, dst_path: &std::path::Path) -> Result<usize> {
    let _ = std::fs::remove_file(dst_path);
    let dst = Connection::open(dst_path)?;
    dst.execute_batch(SCHEMA)?;
    let n = copy_whitelisted(&src.db, &dst)?;
    Ok(n.issues)
}

/// 白名单拷贝核心(export=可信源 / import=不可信源共用;源仅 SELECT)。
fn copy_whitelisted(src: &Connection, dst: &Connection) -> Result<ImportReport> {
    let mut report = ImportReport::default();
    let tx = dst.unchecked_transaction()?;

    // manifest(限定键)
    {
        let mut sel = src.prepare("SELECT key, value FROM manifest WHERE key IN (?1, ?2)")?;
        let rows = sel.query_map(rusqlite::params_from_iter(MANIFEST_KEYS), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (k, v) = row?;
            tx.execute(
                "INSERT OR REPLACE INTO manifest(key,value) VALUES(?1,?2)",
                rusqlite::params![k, v],
            )?;
            if k == "embedding_fp" {
                report.fingerprint = Some(v);
            }
        }
    }

    // issues(id 对齐;title/body 置空)
    {
        let mut sel = src.prepare(&format!("SELECT {ISSUE_COLS} FROM issues"))?;
        let rows = sel.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })?;
        for row in rows {
            let (id, repo, kind, number, state, updated, hash) = row?;
            tx.execute(
                "INSERT OR REPLACE INTO issues(id,repo,kind,number,title,body,state,labels,comments_count,updated_at,embedded_hash)
                 VALUES(?1,?2,?3,?4,'','',?5,NULL,0,?6,?7)",
                rusqlite::params![id, repo, kind, number, state, updated, hash],
            )?;
            report.issues += 1;
        }
    }

    // issues_vec(向量)
    {
        let mut sel = src.prepare("SELECT issue_id, embedding FROM issues_vec")?;
        let rows = sel.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))?;
        for row in rows {
            let (id, blob) = row?;
            tx.execute(
                "INSERT OR REPLACE INTO issues_vec(issue_id, embedding) VALUES(?1,?2)",
                rusqlite::params![id, blob],
            )?;
            report.vectors += 1;
        }
    }

    tx.commit()?;
    Ok(report)
}

/// 安全导入外来骨架 → 本地 store。`expect_fp`:本地嵌入指纹,不匹配即拒。
/// 只读打开外来库、仅 SELECT 白名单列;外来库中的任何表/触发器/DDL 均不执行。
pub fn import_skeleton(
    src_path: &std::path::Path,
    dst: &IssueStore,
    expect_fp: &str,
) -> Result<ImportReport> {
    use rusqlite::OpenFlags;
    let src = Connection::open_with_flags(
        src_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;

    // 1. 表/列结构校验(只 prepare 校验形状)
    for (table, cols) in [
        ("issues", ISSUE_COLS),
        ("issues_vec", "issue_id, embedding"),
        ("manifest", "key, value"),
    ] {
        src.prepare(&format!("SELECT {cols} FROM {table} LIMIT 0"))
            .map_err(|_| Error::Config(format!("骨架缺表/列 [{table}],拒绝导入(格式不明)")))?;
    }

    // 2. 指纹校验(不匹配拒绝,防向量空间混用)
    let fp: Option<String> = src
        .query_row(
            "SELECT value FROM manifest WHERE key='embedding_fp'",
            [],
            |r| r.get(0),
        )
        .ok();
    match &fp {
        Some(f) if f == expect_fp => {}
        Some(f) => {
            return Err(Error::Config(format!(
                "骨架指纹不匹配:文件为 {f},本地为 {expect_fp} —— 换模型请本地重嵌,勿混装"
            )))
        }
        None => {
            return Err(Error::Config(
                "骨架缺 embedding_fp,拒绝导入(格式不明)".to_string(),
            ))
        }
    }

    // 3. 白名单拷贝
    copy_whitelisted(&src, &dst.db)
}

/// 下载骨架(URL → 本地文件);http(s) 才走网络,返回落盘路径。
pub fn download_to(url: &str, dest: &std::path::Path) -> Result<std::path::PathBuf> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Ok(url.into()); // 本地路径直接用
    }
    let resp = ureq::get(url)
        .call()
        .map_err(|e| Error::Io(std::io::Error::other(format!("download {url}: {e}"))))?;
    use std::io::Read;
    let mut bytes = Vec::new();
    resp.into_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| Error::Io(std::io::Error::other(format!("download {url}: {e}"))))?;
    std::fs::write(dest, bytes)?;
    Ok(dest.to_path_buf())
}

/// .gz 则就地解压为 .sqlite,返回可用库路径;否则原样返回。
pub fn gunzip_if_needed(path: &std::path::Path) -> Result<std::path::PathBuf> {
    if path.extension().and_then(|s| s.to_str()) != Some("gz") {
        return Ok(path.to_path_buf());
    }
    use flate2::read::GzDecoder;
    use std::io::Read;
    let f = std::fs::File::open(path)?;
    let mut dec = GzDecoder::new(f);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)?;
    let dst = path.with_extension(""); // *.sqlite.gz → *.sqlite
    std::fs::write(&dst, out)?;
    Ok(dst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedder::EmbeddingFingerprint;

    const FP: &str = "test-model|api|len=512";

    fn seeded_store(tag: &str) -> (std::path::PathBuf, IssueStore) {
        let dir = std::env::temp_dir().join(format!("gh-rag-skel-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = IssueStore::create_fixture(&dir.join("full.sqlite")).unwrap();
        store
            .ensure_embedding_fp(&EmbeddingFingerprint(FP.into()))
            .unwrap();
        let m = crate::store::IssueMeta {
            id: 42,
            repo: "a/b".into(),
            number: 7,
            kind: "pr".into(),
            title: "标题全文".into(),
            body: "正文全文内容".into(),
            state: "closed".into(),
            labels: vec!["bug".into()],
            comments_count: 1,
            comments: Some(vec![crate::github::Comment {
                author: "t8y2".into(),
                body: "评论全文内容".into(),
            }]),
            updated_at: "2026-09-01T00:00:00Z".into(),
        };
        store
            .upsert_batch(&[crate::store::UpsertItem {
                meta: m,
                embedding: vec![1, 2, 3, 4],
                text_hash: "h".into(),
            }])
            .unwrap();
        (dir, store)
    }

    #[test]
    fn export_strips_all_text() {
        let (d, full) = seeded_store("exp");
        let out = d.join("skeleton.sqlite");
        let n = export_skeleton(&full, &out).unwrap();
        assert_eq!(n, 1);
        let sk = Connection::open(&out).unwrap();
        let (title, body): (String, String) = sk
            .query_row("SELECT title, body FROM issues", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert!(title.is_empty() && body.is_empty(), "骨架不得含正文/标题");
        // 评论表必须为空(版权红线自动卡)
        let c: i64 = sk
            .query_row("SELECT COUNT(*) FROM issue_comments", [], |r| r.get(0))
            .unwrap();
        assert_eq!(c, 0, "骨架不得含评论");
    }

    #[test]
    fn import_roundtrip_keeps_vectors_and_fp() {
        let (d, full) = seeded_store("imp");
        let skel = d.join("skeleton.sqlite");
        export_skeleton(&full, &skel).unwrap();

        let fresh = IssueStore::create_fixture(&d.join("fresh.sqlite")).unwrap();
        let r = import_skeleton(&skel, &fresh, FP).unwrap();
        assert_eq!(r.issues, 1);
        assert_eq!(r.vectors, 1);
        assert_eq!(r.fingerprint.as_deref(), Some(FP));
        let m = fresh.get_issue("a/b", 7).unwrap().unwrap();
        assert_eq!(m.kind, "pr");
        assert!(m.body.is_empty(), "导入后正文待本地 sync 补齐");
    }

    #[test]
    fn import_rejects_wrong_fingerprint() {
        let (d, full) = seeded_store("fp");
        let skel = d.join("skeleton.sqlite");
        export_skeleton(&full, &skel).unwrap();
        let fresh = IssueStore::create_fixture(&d.join("f2.sqlite")).unwrap();
        let err = import_skeleton(&skel, &fresh, "other-model|api|len=512").unwrap_err();
        assert!(err.to_string().contains("指纹不匹配"), "got: {err}");
    }

    #[test]
    fn import_ignores_malicious_extras() {
        let (d, full) = seeded_store("evil");
        let skel = d.join("evil.sqlite");
        export_skeleton(&full, &skel).unwrap();
        // 塞恶意内容:额外表 + 触发器(试图在打开时执行)
        let evil = Connection::open(&skel).unwrap();
        evil.execute_batch(
            "CREATE TABLE trap(x);
             CREATE TABLE IF NOT EXISTS pwned(x);
             CREATE TRIGGER t AFTER INSERT ON issues BEGIN INSERT INTO pwned VALUES(1); END;",
        )
        .unwrap();
        drop(evil);

        let fresh = IssueStore::create_fixture(&d.join("f3.sqlite")).unwrap();
        let r = import_skeleton(&skel, &fresh, FP).unwrap();
        assert_eq!(r.issues, 1, "白名单导入不受恶意结构影响");
        // pwned 表绝不能出现在本地库
        let has: i64 = fresh
            .db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name='pwned'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has, 0, "外来结构不得混入本地库");
        // 外来触发器在我们的 INSERT 上若被执行,issues 会有副作用——确认只读语义下未执行:
        // (显式核对:pwned 内容不存在即证明)
        let has_trap: i64 = fresh
            .db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name='trap'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_trap, 0);
    }

    #[test]
    fn import_requires_fingerprint() {
        let (d, full) = seeded_store("nofp");
        let skel = d.join("s.sqlite");
        export_skeleton(&full, &skel).unwrap();
        Connection::open(&skel)
            .unwrap()
            .execute("DELETE FROM manifest", [])
            .unwrap();
        let fresh = IssueStore::create_fixture(&d.join("f4.sqlite")).unwrap();
        assert!(
            import_skeleton(&skel, &fresh, FP).is_err(),
            "无指纹必须拒绝"
        );
    }
}
