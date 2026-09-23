//! 分发链路端到端回归(P0 指纹比对):sync 落库指纹与 fetch 导入指纹必须同源。
//! full_fingerprint 是唯一组装点:sync_repo 落库、fetch 校验、测试路径构造共用。
//! 假 API + 假 embedder,零网络。

use gh_rag_core::embedder::{Embedder, EmbeddingFingerprint};
use gh_rag_core::github::GithubApi;
use gh_rag_core::raw::RawComment;
use gh_rag_core::skeleton::{export_skeleton, import_skeleton};
use gh_rag_core::store::{IssueMeta, IssueStore};
use gh_rag_core::sync::{full_fingerprint, sync_repo, SyncParams};
use gh_rag_core::Result;
use std::time::Duration;

struct FakeGithub {
    issues: Vec<IssueMeta>,
}

impl GithubApi for FakeGithub {
    fn iter_issues(&self, _repo: &str, since: Option<&str>) -> Result<Vec<IssueMeta>> {
        Ok(self
            .issues
            .iter()
            .filter(|m| since.is_none_or(|s| m.updated_at.as_str() >= s))
            .cloned()
            .collect())
    }
    fn iter_comments(&self, _repo: &str, _since: Option<&str>) -> Result<Vec<RawComment>> {
        Ok(vec![])
    }
}

struct FakeEmbedder;

impl Embedder for FakeEmbedder {
    fn embed_texts(&self, texts: &[String]) -> Result<Vec<Vec<u8>>> {
        Ok(texts
            .iter()
            .map(|t| t.len().to_le_bytes().to_vec())
            .collect())
    }
    fn embed_query(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![1.0])
    }
    fn fingerprint(&self) -> EmbeddingFingerprint {
        EmbeddingFingerprint("fake-model|test|len=512".to_string())
    }
}

fn meta(n: i64) -> IssueMeta {
    IssueMeta {
        id: n,
        repo: "t/a".into(),
        number: n,
        kind: "issue".into(),
        title: format!("标题 {n}"),
        body: format!("正文 {n}"),
        state: "open".into(),
        labels: vec!["bug".into()],
        comments_count: 0,
        comments: None,
        author: "alice".into(),
        created_at: "2026-09-01T00:00:00Z".into(),
        updated_at: "2026-09-01T00:00:00Z".into(),
    }
}

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("gh-rag-roundtrip-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn params() -> SyncParams {
    SyncParams {
        batch_size: 8,
        batch_interval: Duration::from_secs(0),
        title_repeats: 2,
        body_max_chars: 2000,
    }
}

/// 模拟 CLI fetch 侧:与 sync 相同 base 指纹 + 相同组装参数 → full 指纹。
fn expect_fp() -> String {
    full_fingerprint("fake-model|test|len=512", &params())
}

#[test]
fn fetch_path_roundtrip_with_synced_fingerprint() {
    let dir = tmp_dir("ok");
    let store = IssueStore::create_fixture(&dir.join("full.sqlite")).unwrap();
    let report = sync_repo(
        &FakeGithub {
            issues: vec![meta(1), meta(2)],
        },
        &store,
        &FakeEmbedder,
        "t/a",
        &params(),
        &dir,
    )
    .unwrap();
    assert_eq!(report.embedded, 2);

    let skel = dir.join("skeleton.sqlite");
    export_skeleton(&store, &skel).unwrap();

    // fetch 侧:同参数构造 expect_fp → 装载成功
    let fresh = IssueStore::create_fixture(&dir.join("index.sqlite")).unwrap();
    let r = import_skeleton(&skel, &fresh, &expect_fp()).unwrap();
    assert_eq!(r.issues, 2);
    assert_eq!(r.vectors, 2);
    assert_eq!(r.fingerprint.as_deref(), Some(expect_fp().as_str()));
}

#[test]
fn wrong_fingerprint_rejected() {
    let dir = tmp_dir("wrongfp");
    let store = IssueStore::create_fixture(&dir.join("full.sqlite")).unwrap();
    sync_repo(
        &FakeGithub {
            issues: vec![meta(1)],
        },
        &store,
        &FakeEmbedder,
        "t/a",
        &params(),
        &dir,
    )
    .unwrap();
    let skel = dir.join("skeleton.sqlite");
    export_skeleton(&store, &skel).unwrap();

    // 裸 embedder 指纹(旧 bug:fetch 未拼参数段)必须被拒
    let fresh = IssueStore::create_fixture(&dir.join("index.sqlite")).unwrap();
    let err = import_skeleton(&skel, &fresh, "fake-model|test|len=512").unwrap_err();
    assert!(err.to_string().contains("指纹不匹配"), "got: {err}");
}

#[test]
fn full_fingerprint_matches_sync_repo_manifest() {
    // full_fingerprint 必须与 sync_repo 实际落库指纹逐字节一致(单一真相源)
    let dir = tmp_dir("manifest");
    let store = IssueStore::create_fixture(&dir.join("full.sqlite")).unwrap();
    sync_repo(
        &FakeGithub {
            issues: vec![meta(1)],
        },
        &store,
        &FakeEmbedder,
        "t/a",
        &params(),
        &dir,
    )
    .unwrap();
    let r = import_skeleton(
        &{
            let skel = dir.join("skeleton.sqlite");
            export_skeleton(&store, &skel).unwrap();
            skel
        },
        &IssueStore::create_fixture(&dir.join("fresh.sqlite")).unwrap(),
        &expect_fp(),
    )
    .unwrap();
    assert_eq!(r.fingerprint.as_deref(), Some(expect_fp().as_str()));
}
