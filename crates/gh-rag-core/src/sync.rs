//! sync 编排:GitHub 拉取 → 内容 hash 增量判定 → 批量嵌入(节流)→ upsert → 游标推进。
//!
//! 增量语义:issue 的 `embedded_hash`(build_text 的 SHA1)与库中一致且向量在 → 跳过重嵌。
//! 与 Python 存量库兼容:Python 同为 sha1(build_text),首跳增量可复用 6385 条已嵌向量。

use crate::embedder::{build_text, Embedder};
use crate::github::GithubApi;
use crate::store::{IssueStore, UpsertItem};
use crate::Result;
use std::time::Duration;

#[derive(Debug, Default, PartialEq)]
pub struct SyncReport {
    pub repo: String,
    pub fetched: usize,
    pub embedded: usize,
    pub skipped: usize,
}

/// 内容 hash:SHA1(title 重复加权 + 正文截断)的 hex。
pub fn text_hash(title: &str, body: &str, title_repeats: usize, body_max_chars: usize) -> String {
    use sha1::{Digest, Sha1};
    let text = build_text(title, body, title_repeats, body_max_chars);
    let mut h = Sha1::new();
    h.update(text.as_bytes());
    format!("{:x}", h.finalize())
}

/// 同步一个仓库。`batch_interval` 为嵌入批次间节流间隔(免费 API 档保守值)。
pub struct SyncParams {
    pub batch_size: usize,
    pub batch_interval: Duration,
    pub title_repeats: usize,
    pub body_max_chars: usize,
}

impl Default for SyncParams {
    fn default() -> Self {
        Self {
            batch_size: 64,
            batch_interval: Duration::from_millis(4000),
            title_repeats: 2,
            body_max_chars: 2000,
        }
    }
}

pub fn sync_repo(
    github: &dyn GithubApi,
    store: &IssueStore,
    embedder: &dyn Embedder,
    repo: &str,
    p: &SyncParams,
) -> Result<SyncReport> {
    store.ensure_embedding_fp(&embedder.fingerprint())?;

    let cursor = store.sync_cursor(repo)?;
    let issues = github.iter_issues(repo, cursor.as_deref())?;
    let mut report = SyncReport {
        repo: repo.to_string(),
        fetched: issues.len(),
        ..Default::default()
    };
    if issues.is_empty() {
        return Ok(report);
    }

    // 增量判定:hash 一致且已有向量 → 跳过
    let mut pending: Vec<(usize, String)> = Vec::new(); // (index, hash)
    for (i, m) in issues.iter().enumerate() {
        let h = text_hash(&m.title, &m.body, p.title_repeats, p.body_max_chars);
        match store.embedded_state(&m.repo, m.number)? {
            Some((old_hash, has_vec)) if old_hash == h && has_vec => report.skipped += 1,
            _ => pending.push((i, h)),
        }
    }

    // 批量嵌入 + 节流
    let mut items: Vec<UpsertItem> = Vec::with_capacity(pending.len());
    for (n, chunk) in pending.chunks(p.batch_size.max(1)).enumerate() {
        if n > 0 {
            std::thread::sleep(p.batch_interval);
        }
        let texts: Vec<String> = chunk
            .iter()
            .map(|(i, _)| {
                let m = &issues[*i];
                build_text(&m.title, &m.body, p.title_repeats, p.body_max_chars)
            })
            .collect();
        let vecs = embedder.embed_texts(&texts)?;
        for ((i, h), v) in chunk.iter().zip(vecs) {
            items.push(UpsertItem {
                meta: issues[*i].clone(),
                embedding: v,
                text_hash: h.clone(),
            });
        }
        report.embedded += chunk.len();
    }

    if !items.is_empty() {
        store.upsert_batch(&items)?;
    }

    // 游标 = 本批最大 updated_at
    let max_updated = issues
        .iter()
        .map(|m| m.updated_at.as_str())
        .max()
        .unwrap_or_default()
        .to_string();
    store.sync_advance(repo, &max_updated)?;
    Ok(report)
}
