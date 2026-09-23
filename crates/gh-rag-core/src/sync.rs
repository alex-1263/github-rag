//! sync 编排:GitHub 拉取 → raw 原始层落盘 → 增量判定(含评论)→ 批量嵌入 → upsert → 游标推进。
//!
//! 数据分层(API 只打一次):
//! - GitHub API → raw 层(JSONL)→ 索引;重建索引只读 raw,零 API
//! - 增量语义:issue 正文/标题/状态 变化(updated_at 游标)+ 评论变化(仓库级评论 since 游标)
//!   两者都转化为 内容 hash 变化,变了才重嵌

use crate::embedder::{build_text_with_comments, Embedder};
use crate::github::GithubApi;
use crate::raw::{RawIssue, RawStore};
use crate::store::{IssueMeta, IssueStore, UpsertItem};
use crate::Result;
use std::collections::HashSet;
use std::time::Duration;

/// 评论聚合参数(嵌入窗口预算:qwen3.7 128K / bge-m3 8K,3K 配额安全)。
pub const COMMENT_QUOTA: usize = 3000;
pub const PER_COMMENT_MAX: usize = 500;

#[derive(Debug, Default, PartialEq)]
pub struct SyncReport {
    pub repo: String,
    pub fetched: usize,
    pub comments_fetched: usize,
    pub embedded: usize,
    pub skipped: usize,
}

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

/// bot 评论过滤:作者以 `[bot]` 结尾(GitHub App 规范)不进嵌入。
fn effective_comments(m: &IssueMeta) -> Vec<(&str, &str)> {
    m.comments
        .as_ref()
        .map(|cs| {
            cs.iter()
                .filter(|c| !c.author.ends_with("[bot]"))
                .map(|c| (c.author.as_str(), c.body.as_str()))
                .collect()
        })
        .unwrap_or_default()
}

/// 内容 hash:SHA1(聚合文本)。评论变化 → hash 变化 → 重嵌。
pub fn text_hash(m: &IssueMeta, p: &SyncParams) -> String {
    use sha1::{Digest, Sha1};
    let text = build_text_with_comments(
        &m.title,
        &m.body,
        &effective_comments(m),
        p.title_repeats,
        p.body_max_chars,
        COMMENT_QUOTA,
        PER_COMMENT_MAX,
    );
    let mut h = Sha1::new();
    h.update(text.as_bytes());
    format!("{:x}", h.finalize())
}

fn to_raw(m: &IssueMeta) -> RawIssue {
    RawIssue {
        number: m.number,
        kind: m.kind.clone(),
        title: m.title.clone(),
        body: m.body.clone(),
        state: m.state.clone(),
        labels: m.labels.clone(),
        comments_count: m.comments_count,
        updated_at: m.updated_at.clone(),
    }
}

/// 同步一个仓库。`raw_home`:raw 层根目录(一般 = gh_rag_home())。
pub fn sync_repo(
    github: &dyn GithubApi,
    store: &IssueStore,
    embedder: &dyn Embedder,
    repo: &str,
    p: &SyncParams,
    raw_home: &std::path::Path,
) -> Result<SyncReport> {
    store.ensure_embedding_fp(&embedder.fingerprint())?;

    let cursor = store.sync_cursor(repo)?;
    let fetched = github.iter_issues(repo, cursor.as_deref())?;
    let comments = github.iter_comments(repo, cursor.as_deref())?;

    // raw 层:合并落盘,读全量(带评论聚合)
    let raws = RawStore::open(raw_home, repo)?;
    let raw_issues: Vec<RawIssue> = fetched.iter().map(to_raw).collect();
    raws.merge(&raw_issues, &comments)?;
    let all = raws.load(repo)?;

    // 受影响集:本批 issue ∪ 有新评论的 issue
    let mut touched: HashSet<i64> = fetched.iter().map(|m| m.number).collect();
    for c in &comments {
        touched.insert(c.issue_number);
    }

    let mut report = SyncReport {
        repo: repo.to_string(),
        fetched: fetched.len(),
        comments_fetched: comments.len(),
        ..Default::default()
    };

    // 增量判定 + 收集待嵌条目
    let mut pending: Vec<(&IssueMeta, String)> = Vec::new();
    for m in &all {
        if !touched.contains(&m.number) {
            continue;
        }
        let h = text_hash(m, p);
        match store.embedded_state(&m.repo, m.number)? {
            Some((old_hash, has_vec)) if old_hash == h && has_vec => {
                // 嵌入跳过,但 raw 带来的新评论仍要落库(bot 过滤只影响嵌入文本)
                store.upsert_meta_and_comments(m)?;
                report.skipped += 1;
            }
            _ => pending.push((m, h)),
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
            .map(|(m, _)| {
                build_text_with_comments(
                    &m.title,
                    &m.body,
                    &effective_comments(m),
                    p.title_repeats,
                    p.body_max_chars,
                    COMMENT_QUOTA,
                    PER_COMMENT_MAX,
                )
            })
            .collect();
        let vecs = embedder.embed_texts(&texts)?;
        for ((m, h), v) in chunk.iter().zip(vecs) {
            items.push(UpsertItem {
                meta: (*m).clone(),
                embedding: v,
                text_hash: h.clone(),
            });
        }
        report.embedded += chunk.len();
    }

    if !items.is_empty() {
        store.upsert_batch(&items)?;
    }

    // 游标 = 本批 issue 最大 updated_at(评论驱动重嵌已由 touched 集保证,不依赖游标)
    let max_updated = fetched
        .iter()
        .map(|m| m.updated_at.as_str())
        .max()
        .unwrap_or_default()
        .to_string();
    if !max_updated.is_empty() {
        store.sync_advance(repo, &max_updated)?;
    }
    Ok(report)
}
