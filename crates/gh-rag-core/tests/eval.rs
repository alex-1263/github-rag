//! 评测全链集成测试:临时库造 query_log + fixture issues → run_eval(FixedJudge,零网络)→ 分数断言;锚定失效路径。

use gh_rag_core::embedder::Embedder;
use gh_rag_core::eval::{run_eval, Anchor, EvalParams, FixedJudge, Judge};
use gh_rag_core::store::IssueStore;
use std::collections::HashMap;

const DIM: usize = 1024;

struct Bag;
fn bag_vec(text: &str) -> Vec<f32> {
    let mut v = vec![0.0f32; DIM];
    for w in text.split_whitespace() {
        let h = mdim(w);
        v[h] += 1.0;
    }
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in v.iter_mut() {
            *x /= n;
        }
    }
    v
}
fn mdim(s: &str) -> usize {
    s.bytes().fold(5381usize, |a, b| {
        (a.wrapping_mul(33).wrapping_add(b as usize)) % DIM
    })
}

impl Embedder for Bag {
    fn embed_texts(&self, texts: &[String]) -> gh_rag_core::Result<Vec<Vec<u8>>> {
        Ok(texts
            .iter()
            .map(|t| bag_vec(t).iter().flat_map(|x| x.to_le_bytes()).collect())
            .collect())
    }
    fn embed_query(&self, t: &str) -> gh_rag_core::Result<Vec<f32>> {
        Ok(bag_vec(t))
    }
    fn fingerprint(&self) -> gh_rag_core::embedder::EmbeddingFingerprint {
        gh_rag_core::embedder::EmbeddingFingerprint("bag-eval-test".into())
    }
}

fn setup() -> (tempdir::Temp, IssueStore) {
    let t = tempdir::Temp::new("eval-it");
    let store = IssueStore::create_fixture(&t.path().join("idx.sqlite")).unwrap();
    let emb = Bag;
    let issues = [
        (
            "o/r",
            1,
            "pdf export fails",
            "export pdf crashes on large files",
        ),
        (
            "o/r",
            2,
            "login loop",
            "oauth redirects back to login forever",
        ),
        (
            "o/r",
            3,
            "pdf rendering blank",
            "pdf shows blank pages after export",
        ),
        (
            "o/r",
            4,
            "unrelated topic",
            "dark mode toggle missing in settings",
        ),
    ];
    for (repo, n, title, body) in issues {
        let text = format!("{title}\n{title}\n{body}");
        let blob = emb.embed_texts(&[text]).unwrap().remove(0);
        store
            .db
            .execute(
                "INSERT INTO issues(repo, number, title, body, state, labels, updated_at) \
                 VALUES (?,?,?,?, 'open','[]','2026-01-01T00:00:00Z')",
                rusqlite::params![repo, n, title, body],
            )
            .unwrap();
        let id = store.db.last_insert_rowid();
        store
            .db
            .execute(
                "INSERT INTO issues_vec(issue_id, embedding) VALUES (?,?)",
                rusqlite::params![id, blob],
            )
            .unwrap();
        store
            .db
            .execute(
                "INSERT INTO issues_fts(rowid, title, body) VALUES (?,?,?)",
                rusqlite::params![id, title, gh_rag_core::cjk::cjk_bigram(body)],
            )
            .unwrap();
    }
    // query_log:两条近 7 天真实查询(同一查询重复出现须去重)+ 一条其他工具 + 一条过期
    store.db.execute_batch(
        "INSERT INTO query_log(ts, tool, query) VALUES (datetime('now'), 'search_issues', 'pdf export fails');
         INSERT INTO query_log(ts, tool, query) VALUES (datetime('now'), 'search_issues', 'pdf export fails');
         INSERT INTO query_log(ts, tool, query) VALUES (datetime('now'), 'search_issues', 'login loop');
         INSERT INTO query_log(ts, tool, query) VALUES (datetime('now'), 'get_issue_context', 'pdf export fails');
         INSERT INTO query_log(ts, tool, query) VALUES (datetime('now', '-30 days'), 'search_issues', 'stale query');",
    ).unwrap();
    (t, store)
}

mod tempdir {
    pub struct Temp(pub std::path::PathBuf);
    impl Temp {
        pub fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let uniq = SEQ.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir()
                .join(format!("gh-rag-eval-{tag}-{}-{uniq}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Temp(dir)
        }
        pub fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// judge:pdf 题下 #1=2、#3=1,其余 0(按标题查表)
fn pdf_judge() -> FixedJudge {
    let mut m = HashMap::new();
    m.insert("pdf export fails".to_string(), 2u8);
    m.insert("pdf rendering blank".to_string(), 1u8);
    m.insert("login loop".to_string(), 0u8);
    m.insert("unrelated topic".to_string(), 0u8);
    FixedJudge::by_title(m)
}

#[test]
fn full_chain_scores_and_dedup() {
    let (_t, store) = setup();
    let report = run_eval(
        &store,
        &Bag,
        &pdf_judge(),
        EvalParams { days: 7, top_k: 5 },
        &[],
    )
    .unwrap();

    // 去重后仅 2 题真实查询
    assert_eq!(report.per_query.len(), 2, "去重+仅 search_issues+近 7 天");
    assert_eq!(report.prompt_hash.len(), 40);

    let pdf = report
        .per_query
        .iter()
        .find(|q| q.query == "pdf export fails")
        .unwrap();
    // 手算:bag 向量下 pdf 两篇应排前;断言分级成绩而非具体排序?排序确定化断言:
    let grades: Vec<u8> = pdf.hits.iter().map(|h| h.grade).collect();
    // pdf export fails 自身标题精确命中 → 应居首位且 = 2
    assert_eq!(pdf.hits[0].grade, 2, "首条应判 2");
    assert!(grades.contains(&1), "应含部分相关 #3");
    assert!(grades.contains(&0), "应含无关 #4 或 #2");

    // 分数与逐题手算一致:pdf 题 grades 形如 [2,1,0,0](2 在首位)
    assert!(report.scores.hit_at5 >= 0.5 && report.scores.hit_at5 <= 1.0);
    assert!(report.scores.ndcg_at5 > 0.0 && report.scores.ndcg_at5 <= 1.0);
    assert!(report.scores.mrr >= 0.5, "两题首位都应相关,mrr ≥ 0.5");
    assert_eq!(
        report.samples.len(),
        5.min(report.per_query.iter().map(|q| q.hits.len()).sum::<usize>())
    );
}

#[test]
fn empty_query_log_errors() {
    let t = tempdir::Temp::new("eval-empty");
    let store = IssueStore::create_fixture(&t.path().join("i.sqlite")).unwrap();
    let e = run_eval(
        &store,
        &Bag,
        &FixedJudge::always(1),
        EvalParams::default(),
        &[],
    )
    .unwrap_err();
    assert!(e.to_string().contains("query_log"));
}

#[test]
fn anchors_consistent_stay_valid() {
    let (_t, store) = setup();
    let anchors = vec![Anchor {
        query: "pdf export fails".into(),
        repo: "o/r".into(),
        number: 1,
        expected_grade: 2,
    }];
    let report = run_eval(
        &store,
        &Bag,
        &pdf_judge(),
        EvalParams { days: 7, top_k: 5 },
        &anchors,
    )
    .unwrap();
    assert!(report.invalid.is_none(), "{:?}", report.invalid);
}

#[test]
fn anchor_only_query_is_judged() {
    let (_t, store) = setup();
    // 锚定查询不在 query_log 里,也必须被评测
    let anchors = vec![Anchor {
        query: "login loop".into(),
        repo: "o/r".into(),
        number: 2,
        expected_grade: 0,
    }];
    let report = run_eval(
        &store,
        &Bag,
        &FixedJudge::always(0),
        EvalParams { days: 1, top_k: 5 },
        &anchors,
    )
    .unwrap();
    assert!(report.per_query.iter().any(|q| q.query == "login loop"));
}

#[test]
fn anchor_drift_marks_invalid() {
    let (_t, store) = setup();
    // 三条锚都是标题精确查询(必命中自身),但裁判 always(0) 与期望(1/2)全不一致 → 100% > 20%
    let anchors = vec![
        Anchor {
            query: "pdf export fails".into(),
            repo: "o/r".into(),
            number: 1,
            expected_grade: 2,
        },
        Anchor {
            query: "login loop".into(),
            repo: "o/r".into(),
            number: 2,
            expected_grade: 1,
        },
        Anchor {
            query: "pdf rendering blank".into(),
            repo: "o/r".into(),
            number: 3,
            expected_grade: 2,
        },
    ];
    let report = run_eval(
        &store,
        &Bag,
        &FixedJudge::always(0),
        EvalParams { days: 7, top_k: 5 },
        &anchors,
    )
    .unwrap();
    assert!(report.invalid.is_some(), "3/3 不一致必须触发 invalid");
    assert!(report.invalid.as_deref().unwrap().contains("20%"));
}

#[test]
fn fixed_judge_reports_reason_passthrough() {
    let j = FixedJudge::always(2);
    let (g, reason) = j.grade_reason("q", "t", "s").unwrap();
    assert_eq!((g, reason), (2, None));
}
