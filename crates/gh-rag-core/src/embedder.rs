//! Embedder trait:向量生产者的唯一抽象。
//!
//! 实现(fastembed 封装,内部即 ort + tokenizers):
//! - `FastEmbedder`:本地 bge-m3(int8),dense 输出 + L2 归一化。
//!   与 Python sentence-transformers(fp32)的对齐度由黄金测试度量。

use crate::Result;

/// 环境指纹:钉进 index.sqlite 的 manifest,防止向量空间混用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingFingerprint(pub String);

/// 嵌入文本组装:标题重复加权 + 正文截断。
/// 必须与 Python 版 `BgeM3Embedder.build_text` 逐字符一致(有单元测试)。
pub fn build_text(title: &str, body: &str, title_repeats: usize, body_max_chars: usize) -> String {
    let t = title.trim();
    let head = format!("{}\n", t).repeat(title_repeats);
    let body_prefix: String = body.chars().take(body_max_chars).collect();
    format!("{}{}", head, body_prefix)
}

pub trait Embedder {
    /// 批量嵌入(建库路径)。输出 float32 小端字节,1024 维,L2 归一化。
    fn embed_texts(&self, texts: &[String]) -> Result<Vec<Vec<u8>>>;

    /// 单条查询嵌入(检索路径)。返回 1024 维 f32,L2 归一化。
    fn embed_query(&self, text: &str) -> Result<Vec<f32>>;

    /// 环境指纹(模型+实现+序列长度),写入 manifest。
    fn fingerprint(&self) -> EmbeddingFingerprint;
}
