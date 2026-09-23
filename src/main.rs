use gh_rag_core::cjk::cjk_bigram;
use gh_rag_core::retrieve::{hybrid_search_with_query, SearchFilter, SearchParams};
use gh_rag_core::store::{IssueStore, IssueMeta, UpsertItem};
use gh_rag_core::embedder::{Embedder, EmbeddingFingerprint};

struct Bag;
const DIM: usize = 64;
fn vec_of(t: &str) -> Vec<f32> {
    let mut v = vec![0.0f32; DIM];
    for w in t.split_whitespace() {
        let mut h: usize = 5381;
        for b in w.bytes() { h = h.wrapping_mul(33).wrapping_add(b as usize); }
        v[h % DIM] += 1.0;
    }
    let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 { for x in v.iter_mut() { *x /= n; } }
    v
}
impl Embedder for Bag {
    fn embed_texts(&self, ts: &[String]) -> gh_rag_core::Result<Vec<Vec<u8>>> {
        Ok(ts.iter().map(|t| vec_of(t).iter().flat_map(|x| x.to_le_bytes()).collect()).collect())
    }
    fn embed_query(&self, t: &str) -> gh_rag_core::Result<Vec<f32>> { Ok(vec_of(t)) }
    fn fingerprint(&self) -> EmbeddingFingerprint { EmbeddingFingerprint("bag|exp|len=64".into()) }
}

fn seed(path: &std::path::Path) -> IssueStore {
    let _ = std::fs::remove_file(path);
    let s = IssueStore::create_fixture(path).unwrap();
    let emb = Bag;
    for (n, title, body, labels) in [
        (1i64, "alpha export pdf", "export pdf broken badly", r#"["bug"]"#),
        (2, "unrelated thing", "totally different topic here", "[]"),
    ] {
        let blob = emb.embed_texts(&[format!("{title} {title} {body}")]).unwrap().remove(0);
        s.db.execute("INSERT INTO issues(repo,number,title,body,state,labels,updated_at) VALUES('a/b',?1,?2,?3,'open',?4,'2026-01-01')",
            rusqlite::params![n, title, body, labels]).unwrap();
        let id = s.db.last_insert_rowid();
        s.db.execute("INSERT INTO issues_vec(issue_id,embedding) VALUES(?1,?2)", rusqlite::params![id, blob]).unwrap();
        s.db.execute("INSERT INTO issues_fts(rowid,title,body) VALUES(?1,?2,?3)",
            rusqlite::params![id, cjk_bigram(title), cjk_bigram(body)]).unwrap();
    }
    s
}

fn main() {
    let dir = std::env::temp_dir().join("ghrag-exp-run");
    let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();

    // (a) labels filter vs FTS leg
    let s = seed(&dir.join("a.sqlite"));
    let q = vec_of("export pdf");
    let f = SearchFilter { labels: Some(vec!["bug".into()]), ..Default::default() };
    let hits = hybrid_search_with_query(&s, &q, "export pdf", &f, 10, &SearchParams::default()).unwrap();
    println!("A labels=[bug] hits: {:?}", hits.iter().map(|h| (h.number, &h.title)).collect::<Vec<_>>());
    println!("A leak (expect #2 absent, present = BUG): {}", hits.iter().any(|h| h.number == 2));

    // (b) dimension mismatch silent truncation
    let mut q1228 = vec![0.5f32; 1228];
    q1228[0] = 1.0;
    let hits = hybrid_search_with_query(&s, &q1228, "x", &SearchFilter::default(), 10, &SearchParams::default()).unwrap();
    println!("B query-dim 1228 vs stored 64: no error, hits={} (silent zip truncation)", hits.len());

    // (c) gunzip_if_needed on non-.gz suffix holding gzip bytes
    let gz_path = dir.join("fetch-download.tmp");
    let mut enc = flate2::write::GzEncoder::new(std::fs::File::create(&gz_path).unwrap(), flate2::Compression::default());
    use std::io::Write; enc.write_all(b"not a sqlite").unwrap(); enc.finish().unwrap();
    let out = gh_rag_core::skeleton::gunzip_if_needed(&gz_path).unwrap();
    println!("C gunzip returned {:?} (same path = gz content NOT decompressed => import fails)", out);

    // (d) follow_up LIKE suffix collision: repos "cat/r" and "at/r"
    let s2 = seed(&dir.join("d.sqlite"));
    s2.log_query("search_issues", "q1", None, &[("cat/r".into(), 7)]).unwrap();
    s2.log_query("search_issues", "q2", None, &[("at/r".into(), 9)]).unwrap();
    let marked = s2.mark_follow_up("at/r", 9).unwrap();
    let (rid, fu): (i64, Option<String>) = s2.db.query_row(
        "SELECT id, follow_up FROM query_log WHERE follow_up IS NOT NULL", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    println!("D mark at/r#9 marked row id={} fu={:?} (id=2 expected; id=1 = wrong row via LIKE suffix)", rid, fu);

    // (e) follow_up double-mark: second get_issue_context re-marks older row?
    // (f) export then fetch-style import into store with fts present: FTS of skeleton store empty, search still fine? skip.

    // (g) issue with NULL body via legacy row -> meta() error?
    s2.db.execute("INSERT INTO issues(repo,number,title,body,state,labels,updated_at) VALUES('a/b',99,'t',NULL,'open','[]','x')", []).unwrap();
    match s2.meta(&[s2.db.last_insert_rowid()]) {
        Ok(m) => println!("G NULL body ok: {:?}", m[0].body),
        Err(e) => println!("G NULL body ERROR: {e}"),
    }

    // (h) upsert_batch comments: meta.comments=None keeps old, Some replaces — verify deleted comment count shrink
    let emb = Bag;
    let blob = emb.embed_texts(&["t x".to_string()]).unwrap().remove(0);
    s2.upsert_batch(&[UpsertItem{ meta: IssueMeta{ id:0, repo:"a/b".into(), number:50, kind:"issue".into(),
        title:"c".into(), body:"b".into(), state:"open".into(), labels:vec![], comments_count:2,
        comments: Some(vec![gh_rag_core::github::Comment{author:"a".into(),body:"c1".into()}]),
        author:"a".into(), created_at:"x".into(), updated_at:"x".into() }, embedding: blob, text_hash:"h50".into()}]).unwrap();
    let blob2 = emb.embed_texts(&["t y".to_string()]).unwrap().remove(0);
    s2.upsert_batch(&[UpsertItem{ meta: IssueMeta{ comments: Some(vec![]), comments_count: 0,
        id:0, repo:"a/b".into(), number:50, kind:"issue".into(), title:"c".into(), body:"b".into(), state:"open".into(),
        labels:vec![], author:"a".into(), created_at:"x".into(), updated_at:"y".into() }, embedding: blob2, text_hash:"h50b".into()}]).unwrap();
    let m = s2.get_issue("a/b", 50).unwrap().unwrap();
    println!("H comments after shrink: {:?}", m.comments);

    // (i) empty query fts
    match s.fts_search("", None, None, 5) { Ok(v) => println!("I empty query ok, hits={}", v.len()), Err(e) => println!("I empty query ERROR: {e}") }
    match s.fts_search("\"", None, None, 5) { Ok(v) => println!("I quote query ok, hits={}", v.len()), Err(e) => println!("I quote query ERROR: {e}") }
}
