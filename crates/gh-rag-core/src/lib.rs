//! gh-rag core:领域逻辑唯一归属。bin(cli/mcp/web)只做壳。
//!
//! 架构纪律见仓库根 AGENTS.md:
//! - IO 全部 trait 化,本 crate 禁止直接网络请求/环境外的隐式状态
//! - 依赖方向单向:cli/mcp/web -> core

pub mod api_embedder;
pub mod cjk;
pub mod config;
pub mod embedder;
pub mod eval;
pub mod github;
pub mod raw;
pub mod relations;
pub mod retrieve;
pub mod skeleton;
pub mod store;
pub mod sync;
pub use embedder::{Embedder, EmbeddingFingerprint};
pub use retrieve::hybrid_search;
pub use store::IssueStore;

/// 全局错误类型(core 内禁止 unwrap)。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("config: {0}")]
    Config(String),
    #[error("embedding fingerprint mismatch: db has {db}, current {current} — rebuild required")]
    FingerprintMismatch { db: String, current: String },

    #[error("向量维度不匹配:索引 {got} 维,查询 {expected} 维 — 嵌入模型/维度已变更,请重建索引")]
    DimensionMismatch { expected: usize, got: usize },

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("eval: {0}")]
    Eval(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
