//! gh-rag core:领域逻辑唯一归属。bin(cli/mcp/web)只做壳。
//!
//! 架构纪律见仓库根 AGENTS.md:
//! - IO 全部 trait 化,本 crate 禁止直接网络请求/环境外的隐式状态
//! - 依赖方向单向:cli/mcp/web -> core

#[cfg(feature = "api")]
pub mod api_embedder;
pub mod embedder;
pub mod retrieve;
pub mod store;
pub use embedder::{Embedder, EmbeddingFingerprint};
pub use retrieve::hybrid_search;
pub use store::IssueStore;

/// 全局错误类型(core 内禁止 unwrap)。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("embedding fingerprint mismatch: db has {db}, current {current} — rebuild required")]
    FingerprintMismatch { db: String, current: String },

    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
