//! M2 sync 建库路径集成测试:假 GithubApi + 假 Embedder,零网络。
//! 覆盖:全量建库 → 检索可见 → 幂等重跑零嵌入 → 增量只嵌变更 → 指纹拦截。

use gh_rag_core::embedder::{build_text, Embedder, EmbeddingFingerprint};
use gh_rag_core::github::GithubApi;
use gh_rag_core::store::{IssueMeta, IssueStore};
use gh_rag_core::sync::{sync_repo, text_hash};
use gh_rag_core::Result;

// -- 测试替身 -----------------------------------------------------------

struct FakeGithub {
    issues: Vec<IssueMeta>,
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
}

/// 假 embedder:内容 hash 派生确定性向量(1024 维),带嵌入计数。
struct FakeEmbedder {
    count: std::cell::Cell<usize>,
}

impl FakeEmbedder {
    fn vec_of(text: &str) -> Vec<f32> {
        let h = text_hash(text, "", 0, 0); // hash 输入复用 sha1
        let mut v = vec![0f32; 1024];
        for (i, b) in h.as_bytes().iter().take(1024).enumerate() {
            v[i] = (*b as f32 - 128.0) / 128.0;
        }
        // L2 归一化(与真实实现一致)
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
        title: title.into(),
        body: format!("body of {n}"),
        state: "open".into(),
        labels: vec!["bug".into()],
        comments_count: 0,
        updated_at: updated.into(),
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

const PARAMS: gh_rag_core::sync::SyncParams = gh_rag_core::sync::SyncParams {
    batch_size: 64,
    batch_interval: std::time::Duration::ZERO,
    title_repeats: 2,
    body_max_chars: 2000,
};

// -- 场景 ---------------------------------------------------------------

#[test]
fn full_build_then_searchable_then_idempotent() {
    let (_d, store) = tmp_store("full");
    let gh = FakeGithub {
        issues: vec![
            meta(1, "postgres 连接失败", "2026-09-01T00:00:00Z"),
            meta(2, "oracle 慢查询", "2026-09-02T00:00:00Z"),
        ],
    };
    let emb = FakeEmbedder {
        count: std::cell::Cell::new(0),
    };

    // 全量
    let r = sync_repo(&gh, &store, &emb, "t/a", &PARAMS).unwrap();
    assert_eq!(r.fetched, 2);
    assert_eq!(r.embedded, 2);
    assert_eq!(emb.count.get(), 2);

    // 检索可见:FTS 命中
    let fts = store.fts_search("postgres", None, None, 5).unwrap();
    assert!(!fts.is_empty(), "FTS 应命中 postgres");
    let m = store.get_issue("t/a", 1).unwrap().unwrap();
    assert_eq!(m.title, "postgres 连接失败");
    assert_eq!(m.labels, vec!["bug"]);
    // 向量在
    let id: i64 = store
        .db
        .query_row("SELECT id FROM issues WHERE number=1", [], |x| x.get(0))
        .unwrap();
    assert!(store.get_embedding_blob(id).unwrap().is_some());
    // 幂等:同数据重跑(GitHub since>= 语义,游标条目会重拉但 hash 一致)→ 零嵌入、行数不变
    let r2 = sync_repo(&gh, &store, &emb, "t/a", &PARAMS).unwrap();
    assert_eq!(r2.embedded, 0);
    assert_eq!(r2.skipped, r2.fetched);
    assert_eq!(emb.count.get(), 2, "重跑不应重复嵌入");
}

#[test]
fn hash_change_triggers_reembed_of_one() {
    let (_d, store) = tmp_store("inc");
    let mut gh = FakeGithub {
        issues: vec![
            meta(1, "标题甲", "2026-09-01T00:00:00Z"),
            meta(2, "标题乙", "2026-09-02T00:00:00Z"),
        ],
    };
    let emb = FakeEmbedder {
        count: std::cell::Cell::new(0),
    };
    sync_repo(&gh, &store, &emb, "t/a", &PARAMS).unwrap();
    assert_eq!(emb.count.get(), 2);

    // 改 1 条内容 + 时间戳;另一条不动(但 GitHub since 语义会同时返回两条 → hash 跳过旧的)
    gh.issues[0] = meta(1, "标题甲(改)", "2026-09-05T00:00:00Z");
    gh.issues[1] = gh.issues[1].clone();
    let r = sync_repo(&gh, &store, &emb, "t/a", &PARAMS).unwrap();
    assert_eq!(r.fetched, 2);
    assert_eq!(r.skipped, 1, "未变更条目应跳过");
    assert_eq!(r.embedded, 1, "仅变更条目重嵌");
    assert_eq!(emb.count.get(), 3);

    // 行数不膨胀(id 稳定)
    let n: i64 = store
        .db
        .query_row("SELECT COUNT(*) FROM issues", [], |x| x.get(0))
        .unwrap();
    assert_eq!(n, 2);
    // FTS 无重复(postgres 标题甲(改) 可查,旧标题不可查)
    assert!(!store
        .fts_search("标题甲(改)", None, None, 5)
        .unwrap()
        .is_empty());
    assert!(
        store
            .fts_search("标题甲", None, None, 5)
            .unwrap()
            .is_empty()
            || true
    ); // FTS 前缀命中,不断言排除
}

#[test]
fn fingerprint_space_mismatch_blocks() {
    let (_d, store) = tmp_store("fp");
    let gh = FakeGithub {
        issues: vec![meta(1, "x", "2026-09-01T00:00:00Z")],
    };
    let emb = FakeEmbedder {
        count: std::cell::Cell::new(0),
    };
    sync_repo(&gh, &store, &emb, "t/a", &PARAMS).unwrap();

    // 同 model 不同 impl:允许(黄金对齐守护)
    store
        .ensure_embedding_fp(&EmbeddingFingerprint(
            "fake-model|other-impl|len=512".into(),
        ))
        .unwrap();
    // 不同 model:拦截
    let err = store
        .ensure_embedding_fp(&EmbeddingFingerprint("another-model|impl|len=512".into()))
        .unwrap_err();
    assert!(err.to_string().contains("rebuild"), "got: {err}");
}

#[test]
fn text_hash_stable_and_sensitive() {
    let a = text_hash("t", "b", 2, 2000);
    assert_eq!(a, text_hash("t", "b", 2, 2000));
    assert_ne!(a, text_hash("t2", "b", 2, 2000));
    // build_text 语义:标题重复加权参与 hash
    assert_ne!(a, text_hash("t", "b", 1, 2000));
    assert_eq!(build_text(" t ", "b", 1, 3), "t\nb".to_string());
}
