//! IssueStore:单文件 SQLite(metadata + BLOB 向量 + FTS5)。
//! M2 实现;M1 阶段仅占位保持 workspace 编译。

use crate::Result;

pub struct IssueStore;

impl IssueStore {
    #[allow(clippy::new_without_default)]
    pub fn new(_db_path: &std::path::Path) -> Result<Self> {
        Ok(Self)
    }
}
