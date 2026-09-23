//! 检索集成测试:fixture 建库(确定性假 embedder)→ 混合检索 → 断言。
//! 不依赖任何模型;假 embedder 用词袋哈希向量,保证同词文本向量相近。

use gh_rag_core::embedder::Embedder;
use gh_rag_core::retrieve::{hybrid_search, SearchFilter, SearchParams};
use gh_rag_core::store::IssueStore;

/// 确定性假 embedder:每个词 hash 到一个维度置 1,L2 归一化。
/// 效果:共享词汇的文本向量余弦接近,完全不同的文本接近正交。
struct BagEmbedder;

const DIM: usize = 1024;

fn bag_vec(text: &str) -> Vec<f32> {
    let mut v = vec![0.0f32; DIM];
    for word in text.split_whitespace() {
        let h = simple_hash(word) % DIM;
        v[h] += 1.0;
    }
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
    v
}

fn simple_hash(s: &str) -> usize {
    let mut h: usize = 5381;
    for b in s.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as usize);
    }
    h
}

impl Embedder for BagEmbedder {
    fn embed_texts(&self, texts: &[String]) -> gh_rag_core::Result<Vec<Vec<u8>>> {
        Ok(texts
            .iter()
            .map(|t| {
                bag_vec(t)
                    .iter()
                    .flat_map(|x| x.to_le_bytes())
                    .collect::<Vec<u8>>()
            })
            .collect())
    }

    fn embed_query(&self, text: &str) -> gh_rag_core::Result<Vec<f32>> {
        Ok(bag_vec(text))
    }

    fn fingerprint(&self) -> gh_rag_core::embedder::EmbeddingFingerprint {
        gh_rag_core::embedder::EmbeddingFingerprint("bag-of-words-test".into())
    }
}

struct FixtureIssue {
    repo: &'static str,
    number: i64,
    title: &'static str,
    body: &'static str,
    state: &'static str,
    labels: &'static [&'static str],
}

fn seed(store: &IssueStore, issues: &[FixtureIssue]) {
    let emb = BagEmbedder;
    for it in issues {
        let text = format!("{}\n{}\n{}", it.title, it.title, it.body);
        let blob = emb.embed_texts(&[text]).unwrap().remove(0);
        store
            .db
            .execute(
                "INSERT INTO issues(repo, number, title, body, state, labels, updated_at) \
                 VALUES (?,?,?,?,?,?, '2026-01-01T00:00:00Z')",
                rusqlite::params![
                    it.repo,
                    it.number,
                    it.title,
                    it.body,
                    it.state,
                    serde_json::to_string(it.labels).unwrap()
                ],
            )
            .unwrap();
        let id = store.db.last_insert_rowid();
        store
            .db
            .execute(
                "INSERT INTO issues_vec(issue_id, embedding) VALUES (?, ?)",
                rusqlite::params![id, blob],
            )
            .unwrap();
        store
            .db
            .execute(
                "INSERT INTO issues_fts(rowid, title, body) VALUES (?,?,?)",
                rusqlite::params![
                    id,
                    gh_rag_core::cjk::cjk_bigram(it.title),
                    gh_rag_core::cjk::cjk_bigram(it.body)
                ],
            )
            .unwrap();
    }
}

fn setup(issues: &[FixtureIssue]) -> IssueStore {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let uniq = SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("gh-rag-test-{}-{uniq}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("fixture.sqlite");
    let _ = std::fs::remove_file(&path);
    let store = IssueStore::create_fixture(&path).unwrap();
    seed(&store, issues);
    store
}

fn fixtures() -> Vec<FixtureIssue> {
    use FixtureIssue as F;
    vec![
        F {
            repo: "acme/web",
            number: 1,
            title: "Login redirect loop after OAuth",
            body: "users are redirected back to login infinitely after oauth session expires",
            state: "open",
            labels: &["bug", "auth"],
        },
        F {
            repo: "acme/web",
            number: 2,
            title: "PDF export missing Chinese characters",
            body: "export renders Chinese as squares, font embedding broken",
            state: "closed",
            labels: &["bug", "export"],
        },
        F {
            repo: "acme/api",
            number: 3,
            title: "Memory leak in connection pool",
            body: "workers grow rss, unclosed connections pile up after 429",
            state: "closed",
            labels: &["bug"],
        },
        F {
            repo: "acme/web",
            number: 4,
            title: "Dark mode toggle resets",
            body: "theme not persisted on refresh",
            state: "open",
            labels: &["bug", "ux"],
        },
    ]
}

/// 全中文无空格文本(缺陷场景:unicode61 把整串 CJK 当单 token)。
fn cjk_fixtures() -> Vec<FixtureIssue> {
    use FixtureIssue as F;
    vec![
        F {
            repo: "acme/cn",
            number: 5,
            title: "导出CSV中文乱码",
            body: "导出报表时中文列全部变成乱码,英文列正常,怀疑编码处理有缺陷",
            state: "open",
            labels: &["bug"],
        },
        F {
            repo: "acme/cn",
            number: 6,
            title: "存储过程无法展开",
            body: "数据库面板里存储过程无法展开查看定义,刷新后依旧",
            state: "closed",
            labels: &["bug"],
        },
    ]
}

#[test]
fn semantic_hit_ranks_relevant_first() {
    let store = setup(&fixtures());
    let hits = hybrid_search(
        &store,
        &BagEmbedder,
        "login redirect oauth loop",
        &SearchFilter::default(),
        2,
        &SearchParams::default(),
    )
    .unwrap();
    assert!(!hits.is_empty());
    assert_eq!(
        hits[0].number,
        1,
        "oauth/login 词袋应把 #1 排第一,得到 {:?}",
        hits.iter().map(|h| h.number).collect::<Vec<_>>()
    );
}

#[test]
fn repo_stats_counts_by_repo() {
    let store = setup(&fixtures());
    let stats = store.repo_stats().unwrap();
    assert_eq!(stats.len(), 2, "两个仓库:acme/web 与 acme/api");
    let web = stats.iter().find(|(r, _, _)| r == "acme/web").unwrap();
    assert_eq!(web.1, 3);
}
#[test]
fn fts_leg_catches_proper_nouns() {
    let store = setup(&fixtures());
    let hits = hybrid_search(
        &store,
        &BagEmbedder,
        "PDF",
        &SearchFilter::default(),
        2,
        &SearchParams::default(),
    )
    .unwrap();
    assert!(
        hits.iter().any(|h| h.number == 2),
        "FTS 必须兜住专有名词 PDF"
    );
}

#[test]
fn repo_filter_limits_scope() {
    let store = setup(&fixtures());
    let hits = hybrid_search(
        &store,
        &BagEmbedder,
        "connection pool memory",
        &SearchFilter {
            repos: Some(vec!["acme/web".into()]),
            ..Default::default()
        },
        5,
        &SearchParams::default(),
    )
    .unwrap();
    assert!(
        hits.iter().all(|h| h.repo == "acme/web"),
        "过滤后不得出现其它仓库"
    );
}

#[test]
fn query_log_records_search() {
    let store = setup(&fixtures());
    let _ = hybrid_search(
        &store,
        &BagEmbedder,
        "anything",
        &SearchFilter::default(),
        2,
        &SearchParams::default(),
    )
    .unwrap();
    let n: i64 = store
        .db
        .query_row("SELECT COUNT(*) FROM query_log", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "检索必须落 query_log");
}

#[test]
fn related_finds_same_topic_and_excludes_self() {
    let store = setup(&fixtures());
    let rel = gh_rag_core::retrieve::find_related(&store, "acme/web", 1, 3, None).unwrap();
    assert!(!rel.is_empty());
    assert!(rel.iter().all(|(_, _, n, _, _)| *n != 1), "不得包含自身");
}

#[test]
fn cjk_single_term_hits_via_fts_leg() {
    let store = setup(&cjk_fixtures());
    let hits = hybrid_search(
        &store,
        &BagEmbedder,
        "乱码",
        &SearchFilter::default(),
        5,
        &SearchParams::default(),
    )
    .unwrap();
    assert!(
        hits.iter().any(|h| h.number == 5),
        "『乱码』必须经 FTS 腿命中『导出CSV中文乱码』,得到 {:?}",
        hits.iter()
            .map(|h| (h.number, h.source))
            .collect::<Vec<_>>()
    );
    let hit = hits.iter().find(|h| h.number == 5).unwrap();
    assert!(
        hit.source.contains("fts"),
        "中文命中必须落在 FTS 腿,实际 source={}",
        hit.source
    );
}

#[test]
fn cjk_multi_term_hits_via_fts_leg() {
    let store = setup(&cjk_fixtures());
    let hits = hybrid_search(
        &store,
        &BagEmbedder,
        "导出 乱码",
        &SearchFilter::default(),
        5,
        &SearchParams::default(),
    )
    .unwrap();
    assert!(
        hits.iter().any(|h| h.number == 5),
        "『导出 乱码』必须命中 #5"
    );
}

#[test]
fn cjk_long_phrase_hits_via_fts_leg() {
    let store = setup(&cjk_fixtures());
    let hits = hybrid_search(
        &store,
        &BagEmbedder,
        "存储过程",
        &SearchFilter::default(),
        5,
        &SearchParams::default(),
    )
    .unwrap();
    assert!(
        hits.iter().any(|h| h.number == 6),
        "『存储过程』必须命中 #6『存储过程无法展开』"
    );
}

#[test]
fn fts_cjk_migration_rebuilds_legacy_index() {
    use std::sync::atomic::{AtomicU64, Ordering};
    let uniq = {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        SEQ.fetch_add(1, Ordering::SeqCst)
    };
    let dir = std::env::temp_dir().join(format!("gh-rag-mig-{}-{uniq}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("legacy.sqlite");
    let _ = std::fs::remove_file(&path);

    // 造旧格式库:issues 表正常,FTS 是 unicode61 裸文本(整串 CJK 单 token)
    {
        let store = IssueStore::create_fixture(&path).unwrap();
        store
            .db
            .execute(
                "INSERT INTO issues(repo, number, title, body, state, labels, updated_at) \
                 VALUES ('acme/cn', 9, '导出CSV中文乱码', '中文列全部乱码', 'open', '[]', '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        let id = store.db.last_insert_rowid();
        store
            .db
            .execute(
                "INSERT INTO issues_fts(rowid, title, body) VALUES (?1, ?2, ?3)",
                rusqlite::params![id, "导出CSV中文乱码", "中文列全部乱码"],
            )
            .unwrap();
    }

    // new() 触发迁移:legacy 索引重建为 bigram
    let store = IssueStore::new(&path).unwrap();
    assert_eq!(
        store.manifest_get("fts_cjk").unwrap().as_deref(),
        Some("1"),
        "迁移必须写 fts_cjk 标记"
    );
    let hits = store.fts_search("乱码", None, None, 10).unwrap();
    assert_eq!(hits.len(), 1, "迁移后中文子串必须可查");

    // 幂等:再次打开不重建(重建也无害,这里验证标记短路)
    let again = IssueStore::new(&path).unwrap();
    let hits = again.fts_search("乱码", None, None, 10).unwrap();
    assert_eq!(hits.len(), 1, "重复打开不得破坏索引");
}
