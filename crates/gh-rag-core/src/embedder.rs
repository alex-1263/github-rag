//! Embedder trait:向量生产者的唯一抽象。
//!
//! 实现(M 按里程碑推进):
//! - `OnnxEmbedder`(M1,feature `golden` 同源):本地 bge-m3 int8,与 Python 版逐位对齐
//! - API 实现(备选,config 切换):硅基流动托管的同款 bge-m3
//!
//! 硬约束:embed_texts(建库)与 embed_query(查询)必须同一实现——向量空间一致性。

use crate::Result;

/// 环境指纹:钉进 index.sqlite 的 manifest,防止向量空间混用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingFingerprint(pub String);

/// 嵌入文本组装:标题重复加权 + 正文截断。
/// 必须与 Python 版 `BgeM3Embedder.build_text` 逐字符一致(有快照测试)。
pub fn build_text(title: &str, body: &str, title_repeats: usize, body_max_chars: usize) -> String {
    let t = title.trim();
    let head = format!("{}\n", t).repeat(title_repeats);
    let body_prefix: String = body.chars().take(body_max_chars).collect();
    format!("{}{}", head, body_prefix)
}

pub trait Embedder {
    /// 批量嵌入(建库路径)。输出 float32 小端字节,1024 维,L2 归一化。
    fn embed_texts(&self, texts: &[String]) -> Result<Vec<Vec<u8>>>;

    /// 单条查询嵌入(检索路径)。返回 1024 维 f32 切片。
    fn embed_query(&self, text: &str) -> Result<Vec<f32>>;

    /// 环境指纹(模型名+版本+序列长度),写入 manifest。
    fn fingerprint(&self) -> EmbeddingFingerprint;
}

/// 本地 ONNX bge-m3(M1 实现)。
#[cfg(feature = "golden")]
pub mod onnx {
    use super::*;

    pub struct OnnxEmbedder {
        pub model_name: String,
        pub max_seq_len: usize,
    }

    impl OnnxEmbedder {
        pub fn new(model_name: impl Into<String>, max_seq_len: usize) -> Self {
            Self {
                model_name: model_name.into(),
                max_seq_len,
            }
        }
    }

    impl Embedder for OnnxEmbedder {
        fn embed_texts(&self, _texts: &[String]) -> Result<Vec<Vec<u8>>> {
            todo!("M1: ONNX batch inference + L2 normalize + f32-LE bytes")
        }

        fn embed_query(&self, _text: &str) -> Result<Vec<f32>> {
            todo!("M1: tokenize -> CLS pool -> L2 normalize")
        }

        fn fingerprint(&self) -> EmbeddingFingerprint {
            // 与 Python 侧 fp 格式对齐:"{model}|st={ver}|len={len}" 的 Rust 侧变体,
            // 由黄金测试之外的 manifest 测试单独覆盖。
            EmbeddingFingerprint(format!(
                "{}|rust-ort|len={}",
                self.model_name, self.max_seq_len
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_text_matches_python_semantics() {
        // 与 Python 行为对齐:标题 trim、重复 N 次、正文按字符截断
        let t = build_text("  标题  ", "正文", 2, 100);
        assert_eq!(t, "标题\n标题\n正文");
    }

    #[test]
    fn build_text_truncates_body_by_chars() {
        let body = "abcdef".repeat(100);
        let t = build_text("t", &body, 1, 10);
        assert!(t.ends_with(&"abcdefghij"));
        assert_eq!(t.chars().count(), 2 + 10); // "t\n" + 10 chars
    }
}
