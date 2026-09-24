//! 查重(check_duplicate)集成测试:fixture 建库(确定性假 embedder,同 tests/search.rs 模式)
//! → 草稿(标题+正文)查重 → 断言。
//! 覆盖:重复标题命中既有 issue 且排前 / 不相关草稿不进 top / repos 过滤 / query_log 工具名。

use gh_rag_core::duplicate::check_duplicate_with_query;
use gh_rag_core::embedder::Embedder;
use gh_rag_core::retrieve::{SearchFilter, SearchParams};
use gh_rag_core::store::IssueStore;

/// 确定性假 embedder:每个词 hash 到一个维度置 1,L2 归一化(同 tests/search.rs)。
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

/// 建库嵌入文本 = 索引侧组装(title_repeats=2 + 正文;同 sync/build_text_with_comments
/// 空评论退化形态)——查重草稿必须走同一组装,向量才可比较。
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
    let dir = std::env::temp_dir().join(format!("gh-rag-dup-test-{}-{uniq}", std::process::id()));
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

/// 查重(与 MCP 同形):draft_text 按索引侧同一组装(title_repeats=2, body_max_chars=2000)
/// → 嵌入 → check_duplicate_with_query。
fn dup(
    store: &IssueStore,
    title: &str,
    body: &str,
    filter: &SearchFilter,
) -> Vec<gh_rag_core::duplicate::DuplicateHit> {
    let text = gh_rag_core::duplicate::draft_text(title, body, 2, 2000);
    let q = BagEmbedder.embed_query(&text).unwrap();
    check_duplicate_with_query(store, &q, title, filter, 5, &SearchParams::default()).unwrap()
}

#[test]
fn duplicate_title_hits_existing_and_ranks_first() {
    let store = setup(&fixtures());
    // 典型重复形态:标题一致(或近似),正文措辞不同
    let hits = dup(
        &store,
        "Login redirect loop after OAuth",
        "after signing in with oauth the app bounces back to the login screen forever, \
         clearing cookies does not help",
        &SearchFilter::default(),
    );
    assert!(!hits.is_empty(), "重复标题草稿必须命中既有 issue");
    assert_eq!(hits[0].number, 1, "重复标题应排第一,得到 {:?}", hits);
    assert_eq!(hits[0].repo, "acme/web");
    assert_eq!(hits[0].state, "open");
    assert!(
        hits[0].title_sim >= 0.99,
        "完全相同标题的 title_sim 应为 1.0,得到 {}",
        hits[0].title_sim
    );
    for h in &hits {
        assert!((0.0..=1.0).contains(&h.title_sim), "title_sim 必须在 [0,1]");
        assert!(h.score > 0.0, "score 语义不变(检索分)");
    }
}

#[test]
fn cjk_duplicate_title_hits() {
    use FixtureIssue as F;
    let store = setup(&[
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
    ]);
    let hits = dup(
        &store,
        "导出CSV中文乱码",
        "升级到最新版本后导出依然是乱码,重装字体无效",
        &SearchFilter::default(),
    );
    assert!(!hits.is_empty(), "中文重复标题必须命中");
    assert_eq!(
        hits[0].number, 5,
        "中文标题 bigram 相似度应把 #5 排前,得到 {:?}",
        hits
    );
    assert!(hits[0].title_sim >= 0.99);
}

#[test]
fn unrelated_draft_returns_nothing() {
    let store = setup(&fixtures());
    // 与库内任何 issue 无词汇/词面重叠的草稿:不应给出任何"疑似重复"
    let hits = dup(
        &store,
        "Improve pagination performance",
        "loading five hundred rows takes seconds, switch to cursor based pagination and batch fetch",
        &SearchFilter::default(),
    );
    assert!(
        hits.is_empty(),
        "不相关草稿不应进 top,却得到 {:?}",
        hits.iter()
            .map(|h| (h.number, h.title_sim))
            .collect::<Vec<_>>()
    );
}

#[test]
fn repos_filter_scopes_duplicates() {
    let mut fx = fixtures();
    fx.push(FixtureIssue {
        repo: "acme/api",
        number: 7,
        title: "Login redirect loop after OAuth",
        body: "oauth callback loops forever on the api gateway after token refresh",
        state: "open",
        labels: &["bug"],
    });
    let store = setup(&fx);
    let draft_body =
        "after signing in with oauth the app sends users back to the login page in a loop";

    // 限定 acme/api:命中同仓 #7,acme/web 的 #1 不得出现
    let api = SearchFilter {
        repos: Some(vec!["acme/api".to_string()]),
        ..Default::default()
    };
    let hits = dup(&store, "Login redirect loop after OAuth", draft_body, &api);
    assert!(!hits.is_empty(), "限定仓内存在同题 issue,必须命中");
    assert!(
        hits.iter().all(|h| h.repo == "acme/api"),
        "repos 过滤必须排除 acme/web,得到 {:?}",
        hits.iter().map(|h| h.repo.clone()).collect::<Vec<_>>()
    );
    assert_eq!(hits[0].number, 7);

    // 反向对照:同一草稿限定 acme/web → 命中 #1,不返回 acme/api 的 #7
    let web = SearchFilter {
        repos: Some(vec!["acme/web".to_string()]),
        ..Default::default()
    };
    let hits = dup(&store, "Login redirect loop after OAuth", draft_body, &web);
    assert!(!hits.is_empty());
    assert!(
        hits.iter().all(|h| h.repo == "acme/web"),
        "repos 过滤必须排除 acme/api,得到 {:?}",
        hits.iter().map(|h| h.repo.clone()).collect::<Vec<_>>()
    );
    assert_eq!(hits[0].number, 1);
}

#[test]
fn query_log_labels_check_duplicate() {
    let store = setup(&fixtures());
    let _ = dup(
        &store,
        "Login redirect loop after OAuth",
        "after signing in with oauth the app bounces back to the login screen forever",
        &SearchFilter::default(),
    );
    let n: i64 = store
        .db
        .query_row(
            "SELECT COUNT(*) FROM query_log WHERE tool='check_duplicate'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "查重必须落 query_log 且工具名为 check_duplicate");
    let polluted: i64 = store
        .db
        .query_row(
            "SELECT COUNT(*) FROM query_log WHERE tool<>'check_duplicate'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        polluted, 0,
        "查重不得以其他工具名落库(eval 题库只认 search_issues)"
    );
}
