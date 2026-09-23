//! M2 sync 建库路径集成测试:假 GithubApi + 假 Embedder,零网络。
//! 覆盖:全量建库 → 检索可见 → 幂等重跑零嵌入 → 增量只嵌变更 → 指纹拦截
//!      → 评论驱动重嵌 + 评论落库 + bot 过滤。

use gh_rag_core::embedder::{build_text, Embedder, EmbeddingFingerprint};
use gh_rag_core::github::GithubApi;
use gh_rag_core::raw::RawComment;
use gh_rag_core::store::{IssueMeta, IssueStore};
use gh_rag_core::sync::{sync_repo, text_hash, SyncParams};
use gh_rag_core::Result;
use std::time::Duration;

// -- 测试替身 -----------------------------------------------------------

struct FakeGithub {
    issues: Vec<IssueMeta>,
    comments: Vec<RawComment>,
}

impl GithubApi for FakeGithub {
    fn iter_issues(&self, _repo: &str, since: Option<&str>) -> Result<Vec<IssueMeta>> {
        // GitHub 真实语义:since 返回 updated_at >= since
        Ok(self
            .issues
            .iter()
            .filter(|m| since.is_none_or(|s| m.updated_at.as_str() >= s))
            .cloned()
            .collect())
    }

    fn iter_comments(&self, _repo: &str, since: Option<&str>) -> Result<Vec<RawComment>> {
        Ok(self
            .comments
            .iter()
            .filter(|c| since.is_none_or(|s| c.created_at.as_str() >= s))
            .cloned()
            .collect())
    }
}

/// 假 embedder:内容 hash 派生确定性向量(1024 维),带嵌入计数。
struct FakeEmbedder {
    count: std::cell::Cell<usize>,
}

impl FakeEmbedder {
    fn vec_of(text: &str) -> Vec<f32> {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut h);
        let seed = h.finish();
        let mut v = vec![0f32; 1024];
        for (i, b) in seed.to_le_bytes().iter().cycle().take(1024).enumerate() {
            v[i] = (*b as f32 - 128.0) / 128.0 + (i as f32) * 1e-6;
        }
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        for x in &mut v {
            *x /= n;
        }
        v
    }
}

impl Embedder for FakeEmbedder {
    fn embed_texts(&self, texts: &[String]) -> Result<Vec<Vec<u8>>> {
        self.count.set(self.count.get() + texts.len());
        Ok(texts
            .iter()
            .map(|t| {
                Self::vec_of(t)
                    .iter()
                    .flat_map(|f| f.to_le_bytes())
                    .collect()
            })
            .collect())
    }
    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        Ok(Self::vec_of(text))
    }
    fn fingerprint(&self) -> EmbeddingFingerprint {
        EmbeddingFingerprint("fake-model|test|len=512".to_string())
    }
}

fn meta(n: i64, title: &str, updated: &str) -> IssueMeta {
    IssueMeta {
        id: n,
        repo: "t/a".into(),
        number: n,
        kind: "issue".into(),
        title: title.into(),
        body: format!("body of {n}"),
        state: "open".into(),
        labels: vec!["bug".into()],
        comments_count: 0,
        comments: None,
        updated_at: updated.into(),
    }
}

fn rc(id: i64, n: i64, author: &str, body: &str, created: &str) -> RawComment {
    RawComment {
        id,
        issue_number: n,
        author: author.into(),
        body: body.into(),
        created_at: created.into(),
    }
}

fn tmp_store(tag: &str) -> (std::path::PathBuf, IssueStore) {
    let dir = std::env::temp_dir().join(format!("gh-rag-sync-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("index.sqlite");
    let store = IssueStore::create_fixture(&path).unwrap();
    (dir, store)
}

fn params() -> SyncParams {
    SyncParams {
        batch_size: 64,
        batch_interval: Duration::ZERO,
        title_repeats: 2,
        body_max_chars: 2000,
    }
}

fn sync(
    gh: &FakeGithub,
    store: &IssueStore,
    emb: &FakeEmbedder,
    home: &std::path::Path,
) -> gh_rag_core::sync::SyncReport {
    sync_repo(gh, store, emb, "t/a", &params(), home).unwrap()
}

// -- 场景 ---------------------------------------------------------------

#[test]
fn full_build_then_searchable_then_idempotent() {
    let (d, store) = tmp_store("full");
    let gh = FakeGithub {
        issues: vec![
            meta(1, "postgres 连接失败", "2026-09-01T00:00:00Z"),
            meta(2, "oracle 慢查询", "2026-09-02T00:00:00Z"),
        ],
        comments: vec![],
    };
    let emb = FakeEmbedder {
        count: std::cell::Cell::new(0),
    };

    // 全量
    let r = sync(&gh, &store, &emb, &d);
    assert_eq!(r.fetched, 2);
    assert_eq!(r.embedded, 2);
    assert_eq!(emb.count.get(), 2);

    // 检索可见:FTS 命中
    let fts = store.fts_search("postgres", None, None, 5).unwrap();
    assert!(!fts.is_empty(), "FTS 应命中 postgres");
    let m = store.get_issue("t/a", 1).unwrap().unwrap();
    assert_eq!(m.title, "postgres 连接失败");
    assert_eq!(m.labels, vec!["bug"]);
    let id: i64 = store
        .db
        .query_row("SELECT id FROM issues WHERE number=1", [], |x| x.get(0))
        .unwrap();
    assert!(store.get_embedding_blob(id).unwrap().is_some());

    // 幂等:同数据重跑(since>= 语义,游标条目重拉但 hash 一致)→ 零嵌入
    let r2 = sync(&gh, &store, &emb, &d);
    assert_eq!(r2.embedded, 0);
    assert_eq!(r2.skipped, r2.fetched);
    assert_eq!(emb.count.get(), 2, "重跑不应重复嵌入");
}

#[test]
fn hash_change_triggers_reembed_of_one() {
    let (d, store) = tmp_store("inc");
    let mut gh = FakeGithub {
        issues: vec![
            meta(1, "标题甲", "2026-09-01T00:00:00Z"),
            meta(2, "标题乙", "2026-09-02T00:00:00Z"),
        ],
        comments: vec![],
    };
    let emb = FakeEmbedder {
        count: std::cell::Cell::new(0),
    };
    sync(&gh, &store, &emb, &d);
    assert_eq!(emb.count.get(), 2);

    // 改 1 条内容 + 时间戳;另一条不动(重拉但 hash 跳过)
    gh.issues[0] = meta(1, "标题甲(改)", "2026-09-05T00:00:00Z");
    let r = sync(&gh, &store, &emb, &d);
    assert_eq!(r.fetched, 2);
    assert_eq!(r.skipped, 1, "未变更条目应跳过");
    assert_eq!(r.embedded, 1, "仅变更条目重嵌");
    assert_eq!(emb.count.get(), 3);

    let n: i64 = store
        .db
        .query_row("SELECT COUNT(*) FROM issues", [], |x| x.get(0))
        .unwrap();
    assert_eq!(n, 2, "行数不膨胀(id 稳定)");
    assert!(!store
        .fts_search("标题甲(改)", None, None, 5)
        .unwrap()
        .is_empty());
}

#[test]
fn fingerprint_space_mismatch_blocks() {
    let (d, store) = tmp_store("fp");
    let gh = FakeGithub {
        issues: vec![meta(1, "x", "2026-09-01T00:00:00Z")],
        comments: vec![],
    };
    let emb = FakeEmbedder {
        count: std::cell::Cell::new(0),
    };
    sync(&gh, &store, &emb, &d);

    // 同 model 不同 impl:允许(黄金对齐守护)——完整分段指纹
    store
        .ensure_embedding_fp(&EmbeddingFingerprint(
            "fake-model|other-impl|len=512|tr=2|body=2000|cq=3000|cc=500".into(),
        ))
        .unwrap();
    // 不同 model:拦截
    let err = store
        .ensure_embedding_fp(&EmbeddingFingerprint(
            "another-model|impl|len=512|tr=2|body=2000|cq=3000|cc=500".into(),
        ))
        .unwrap_err();
    assert!(err.to_string().contains("rebuild"), "got: {err}");
}

#[test]
fn same_params_rerun_passes() {
    let (d, store) = tmp_store("fpok");
    let gh = FakeGithub {
        issues: vec![meta(1, "x", "2026-09-01T00:00:00Z")],
        comments: vec![],
    };
    let emb = FakeEmbedder {
        count: std::cell::Cell::new(0),
    };
    sync(&gh, &store, &emb, &d);
    // 同参数重跑(第二次 sync 内部走 ensure)→ 放行
    let r = sync_repo(&gh, &store, &emb, "t/a", &params(), &d).unwrap();
    assert_eq!(r.embedded, 0);
}

#[test]
fn assembly_param_change_blocks_with_message() {
    let (d, store) = tmp_store("fpasm");
    let gh = FakeGithub {
        issues: vec![meta(1, "x", "2026-09-01T00:00:00Z")],
        comments: vec![],
    };
    let emb = FakeEmbedder {
        count: std::cell::Cell::new(0),
    };
    sync(&gh, &store, &emb, &d);

    // title_repeats 变更 → sync 被拦,报文含「组装参数」
    let mut p2 = params();
    p2.title_repeats = 3;
    let err = sync_repo(&gh, &store, &emb, "t/a", &p2, &d).unwrap_err();
    assert!(err.to_string().contains("组装参数"), "got: {err}");
}

#[test]
fn old_format_fingerprint_migrates_without_reembed() {
    let (d, store) = tmp_store("fpmig");
    // 手工写入旧格式指纹(存量库)
    store
        .db
        .execute(
            "INSERT OR REPLACE INTO manifest(key,value) VALUES('embedding_fp','fake-model|api|len=512')",
            [],
        )
        .unwrap();
    let gh = FakeGithub {
        issues: vec![meta(1, "x", "2026-09-01T00:00:00Z")],
        comments: vec![],
    };
    let emb = FakeEmbedder {
        count: std::cell::Cell::new(0),
    };
    // 旧格式 + 同空间 → 放行迁移
    sync(&gh, &store, &emb, &d);
    let fp: String = store
        .db
        .query_row(
            "SELECT value FROM manifest WHERE key='embedding_fp'",
            [],
            |x| x.get(0),
        )
        .unwrap();
    assert!(fp.contains("tr="), "指纹应覆写为新格式: {fp}");
}

#[test]
fn text_hash_stable_and_sensitive() {
    let m = meta(1, "t", "2026-09-01T00:00:00Z");
    let a = text_hash(&m, &params());
    assert_eq!(a, text_hash(&m, &params()));
    let mut m2 = m.clone();
    m2.title = "t2".into();
    assert_ne!(a, text_hash(&m2, &params()));
    assert_eq!(build_text(" t ", "b", 1, 3), "t\nb".to_string());
}

#[test]
fn comments_drive_reembed_and_land_in_db() {
    let (d, store) = tmp_store("cmt");
    let mut gh = FakeGithub {
        issues: vec![meta(1, "查询超时", "2026-09-01T00:00:00Z")],
        comments: vec![],
    };
    let emb = FakeEmbedder {
        count: std::cell::Cell::new(0),
    };
    sync(&gh, &store, &emb, &d);
    assert_eq!(emb.count.get(), 1);

    // 新评论到达(不改 issue 本体)→ hash 变化 → 重嵌;评论落库可读
    gh.comments = vec![rc(
        9,
        1,
        "alice",
        "根因是连接池耗尽,workaround 是调大 max_connections",
        "2026-09-05T00:00:00Z",
    )];
    let r = sync(&gh, &store, &emb, &d);
    assert_eq!(r.embedded, 1, "评论变化应触发重嵌");
    assert_eq!(emb.count.get(), 2);
    let m = store.get_issue("t/a", 1).unwrap().unwrap();
    let cs = m.comments.unwrap();
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].author, "alice");
    assert!(cs[0].body.contains("连接池"));

    // bot 评论不进嵌入(hash 不变 → 跳过),但落库保留(可读)
    gh.comments.push(rc(
        10,
        1,
        "dbx-bot[bot]",
        "auto close",
        "2026-09-06T00:00:00Z",
    ));
    let r2 = sync(&gh, &store, &emb, &d);
    assert_eq!(r2.embedded, 0, "bot 评论不应触发重嵌");
    let cs2 = store
        .get_issue("t/a", 1)
        .unwrap()
        .unwrap()
        .comments
        .unwrap();
    assert_eq!(cs2.len(), 2, "bot 评论仍落库(检索可见,嵌入不含)");
}

/// 两页分页假 API:覆盖 issues_pages(每页即回调),第一页回调后断言已落盘(流式持久)。
struct TwoPageGithub {
    home: std::path::PathBuf,
}

impl GithubApi for TwoPageGithub {
    fn iter_issues(&self, _repo: &str, _since: Option<&str>) -> Result<Vec<IssueMeta>> {
        unimplemented!("sync_repo 应走 issues_pages 流式路径")
    }
    fn issues_pages(
        &self,
        _repo: &str,
        _since: Option<&str>,
        f: &mut dyn FnMut(Vec<IssueMeta>) -> Result<()>,
    ) -> Result<()> {
        f(vec![
            meta(1, "第一页甲", "2026-09-01T00:00:00Z"),
            meta(2, "第一页乙", "2026-09-02T00:00:00Z"),
        ])?;
        // 第一页回调返回后必须已持久化(中途崩溃不丢)
        let raws = gh_rag_core::raw::RawStore::open(&self.home, "t/a").unwrap();
        assert_eq!(raws.load("t/a").unwrap().len(), 2, "第一页应已落 raw 层");
        f(vec![
            meta(3, "第二页丙", "2026-09-03T00:00:00Z"),
            meta(4, "第二页丁", "2026-09-04T00:00:00Z"),
        ])?;
        Ok(())
    }
}

#[test]
fn issues_pages_streams_each_page_into_raw_layer() {
    let (d, store) = tmp_store("pages");
    let gh = TwoPageGithub { home: d.clone() };
    let emb = FakeEmbedder {
        count: std::cell::Cell::new(0),
    };

    let r = sync_repo(&gh, &store, &emb, "t/a", &params(), &d).unwrap();
    assert_eq!(r.fetched, 4, "两页各 2 条");
    assert_eq!(r.embedded, 4);

    // sync 结束后 raw 层全量 4 条
    let raws = gh_rag_core::raw::RawStore::open(&d, "t/a").unwrap();
    let all = raws.load("t/a").unwrap();
    assert_eq!(all.len(), 4);
    let mut nums: Vec<i64> = all.iter().map(|m| m.number).collect();
    nums.sort_unstable();
    assert_eq!(nums, vec![1, 2, 3, 4]);

    // 索引侧也可读
    assert!(store.get_issue("t/a", 4).unwrap().is_some());
    assert_eq!(emb.count.get(), 4);
}

#[test]
fn raw_layer_survives_rebuild_without_api() {
    // 重建场景:raw 层已有数据,github 断流(空响应)也应能从 raw 全量重建
    let (d, store) = tmp_store("rebuild");
    let gh = FakeGithub {
        issues: vec![meta(1, "postgres 失败", "2026-09-01T00:00:00Z")],
        comments: vec![rc(1, 1, "a", "讨论内容", "2026-09-01T00:00:00Z")],
    };
    let emb = FakeEmbedder {
        count: std::cell::Cell::new(0),
    };
    sync(&gh, &store, &emb, &d);
    assert_eq!(emb.count.get(), 1);

    // 新库(模拟 rm index.sqlite 后重建):API 无新增(raw 里全有)→ 触及集为空
    let fresh = IssueStore::create_fixture(&d.join("index2.sqlite")).unwrap();
    let empty_gh = FakeGithub {
        issues: vec![],
        comments: vec![],
    };
    let r = sync(&empty_gh, &fresh, &emb, &d);
    assert_eq!(r.fetched, 0, "API 无新增");
    // 注:触及集为空 → 不会重嵌。全量重建入口 = 清空 touched(未来 rebuild 命令);
    // 此处验证 raw 层就位:直接 load
    let raws = gh_rag_core::raw::RawStore::open(&d, "t/a").unwrap();
    let all = raws.load("t/a").unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].comments.as_ref().unwrap().len(), 1);
}
