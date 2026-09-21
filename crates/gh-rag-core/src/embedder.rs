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

#[cfg(any(feature = "fp32", feature = "int8"))]
pub mod onnx {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Mutex;

    fn gh_rag_home() -> PathBuf {
        if let Ok(h) = std::env::var("GH_RAG_HOME") {
            return PathBuf::from(h);
        }
        #[cfg(windows)]
        let key = "USERPROFILE";
        #[cfg(not(windows))]
        let key = "HOME";
        std::env::var(key)
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".gh-rag") // 与 Python 侧 DATA_DIR 对齐
    }

    /// fastembed 驱动的 bge-m3(int8)。
    #[cfg(feature = "int8")]
    /// embed 需要 &mut,trait 是 &self → Mutex 串行化(嵌入天然批量,MCP 只读并发低)。
    pub struct OnnxEmbedder {
        inner: Mutex<fastembed::Bgem3Embedding>,
        max_seq_len: usize,
    }

    #[cfg(feature = "int8")]
    impl OnnxEmbedder {
        pub fn new(max_seq_len: usize) -> Result<Self> {
            // 本地模型优先(离线/镜像网络);否则回退 HF 自动下载
            let local = gh_rag_home().join("models").join("bge-m3-int8");
            let inner = if local.join("model_quantized.onnx").exists() {
                let model_file = local.join("model_quantized.onnx");
                let files = fastembed::TokenizerFiles {
                    tokenizer_file: std::fs::read(local.join("tokenizer.json"))?,
                    config_file: std::fs::read(local.join("config.json"))?,
                    special_tokens_map_file: std::fs::read(local.join("special_tokens_map.json"))?,
                    tokenizer_config_file: std::fs::read(local.join("tokenizer_config.json"))?,
                };
                let opts = fastembed::InitOptionsUserDefined::new().with_max_length(max_seq_len);
                fastembed::Bgem3Embedding::try_new_from_path(&model_file, files, opts)
                    .map_err(fastembed_err)?
            } else {
                let opts = fastembed::Bgem3InitOptions::new(fastembed::Bgem3Model::default())
                    .with_max_length(max_seq_len)
                    .with_cache_dir(gh_rag_home().join("models").join("fastembed-cache"))
                    .with_show_download_progress(true);
                fastembed::Bgem3Embedding::try_new(opts).map_err(fastembed_err)?
            };
            Ok(Self {
                inner: Mutex::new(inner),
                max_seq_len,
            })
        }
        fn embed_dense(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            let mut model = self
                .inner
                .lock()
                .map_err(|_| crate::Error::Io(std::io::Error::other("embedder mutex poisoned")))?;
            let out = model
                .embed(texts, Some(32))
                .map_err(|e| crate::Error::Io(std::io::Error::other(format!("embed: {e}"))))?;
            Ok(out.dense.into_iter().map(l2_normalize).collect())
        }
    }

    #[cfg(feature = "int8")]
    fn fastembed_err(e: fastembed::Error) -> crate::Error {
        crate::Error::Io(std::io::Error::other(format!("fastembed: {e}")))
    }

    fn l2_normalize(mut v: Vec<f32>) -> Vec<f32> {
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in v.iter_mut() {
                *x /= norm;
            }
        }
        v
    }

    fn to_le_bytes(v: &[f32]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(v.len() * 4);
        for x in v {
            buf.extend_from_slice(&x.to_le_bytes());
        }
        buf
    }

    #[cfg(feature = "int8")]
    impl Embedder for OnnxEmbedder {
        fn embed_texts(&self, texts: &[String]) -> Result<Vec<Vec<u8>>> {
            let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
            Ok(self
                .embed_dense(&refs)?
                .into_iter()
                .map(|v| to_le_bytes(&v))
                .collect())
        }

        fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
            Ok(self
                .embed_dense(&[text])?
                .into_iter()
                .next()
                .unwrap_or_default())
        }

        fn fingerprint(&self) -> EmbeddingFingerprint {
            // 诚实标注:int8 与 Python fp32 的对齐度由黄金测试度量并记录
            EmbeddingFingerprint(format!(
                "BAAI/bge-m3|fastembed-int8|len={}",
                self.max_seq_len
            ))
        }
    }

    /// fp32 直连(Xenova 单输出 model.onnx + external data)。
    #[cfg(feature = "fp32")]
    /// M1 对齐证明:与 Python sentence-transformers 逐位对齐(黄金阈值 0.999)。
    pub struct Fp32Embedder {
        session: Mutex<ort::session::Session>,
        tokenizer: tokenizers::Tokenizer,
        dim: usize,
        max_seq_len: usize,
        need_type_ids: bool,
    }

    #[cfg(feature = "fp32")]
    impl Fp32Embedder {
        pub fn new(max_seq_len: usize) -> Result<Self> {
            let dir = gh_rag_home().join("models").join("bge-m3");
            let graph = dir.join("model.onnx");
            if !graph.exists() {
                return Err(crate::Error::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "missing {} (Xenova fp32: model.onnx + model.onnx_data + tokenizer.json)",
                        graph.display()
                    ),
                )));
            }
            let builder = ort::session::Session::builder().map_err(ort_err)?;
            let builder = builder
                .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
                .map_err(ort_err)?;
            let mut builder = builder
                .with_intra_threads(
                    std::thread::available_parallelism()
                        .map(|n| n.get())
                        .unwrap_or(8),
                )
                .map_err(ort_err)?;
            let session = builder.commit_from_file(&graph).map_err(ort_err)?;

            let need_type_ids = session
                .inputs()
                .iter()
                .any(|i| i.name() == "token_type_ids");

            let mut tokenizer = tokenizers::Tokenizer::from_file(dir.join("tokenizer.json"))
                .map_err(|e| crate::Error::Io(std::io::Error::other(format!("tokenizer: {e}"))))?;
            tokenizer
                .with_truncation(Some(tokenizers::TruncationParams {
                    max_length: max_seq_len,
                    ..Default::default()
                }))
                .map_err(|e| crate::Error::Io(std::io::Error::other(format!("truncation: {e}"))))?;

            Ok(Self {
                session: Mutex::new(session),
                tokenizer,
                dim: 1024,
                max_seq_len,
                need_type_ids,
            })
        }

        fn encode_one(&self, text: &str) -> Result<Vec<f32>> {
            use ndarray::Array2;
            use ort::value::Value;

            let enc = self
                .tokenizer
                .encode(text, true)
                .map_err(|e| crate::Error::Io(std::io::Error::other(format!("encode: {e}"))))?;
            let len = enc.get_ids().len();
            if len == 0 {
                return Ok(vec![0.0; self.dim]);
            }
            let ids = Array2::from_shape_vec(
                (1, len),
                enc.get_ids().iter().map(|&v| v as i64).collect::<Vec<_>>(),
            )
            .map_err(|e| crate::Error::Io(std::io::Error::other(e.to_string())))?;
            let mask = Array2::from_shape_vec(
                (1, len),
                enc.get_attention_mask()
                    .iter()
                    .map(|&v| v as i64)
                    .collect::<Vec<_>>(),
            )
            .map_err(|e| crate::Error::Io(std::io::Error::other(e.to_string())))?;

            let mut inputs: Vec<(
                std::borrow::Cow<'static, str>,
                ort::session::SessionInputValue<'static>,
            )> = Vec::new();
            inputs.push((
                "input_ids".into(),
                Value::from_array(ids).map_err(ort_err)?.into(),
            ));
            inputs.push((
                "attention_mask".into(),
                Value::from_array(mask).map_err(ort_err)?.into(),
            ));
            if self.need_type_ids {
                let tids = Array2::from_shape_vec(
                    (1, len),
                    enc.get_type_ids()
                        .iter()
                        .map(|&v| v as i64)
                        .collect::<Vec<_>>(),
                )
                .map_err(|e| crate::Error::Io(std::io::Error::other(e.to_string())))?;
                inputs.push((
                    "token_type_ids".into(),
                    Value::from_array(tids).map_err(ort_err)?.into(),
                ));
            }

            let mut session = self
                .session
                .lock()
                .map_err(|_| crate::Error::Io(std::io::Error::other("session mutex poisoned")))?;
            let outputs = session.run(inputs).map_err(ort_err)?;
            let out = &outputs[0];
            let (shape, data) = out.try_extract_tensor::<f32>().map_err(ort_err)?;
            // last_hidden_state [1, seq, dim] → CLS = 行首 dim 个
            let d = shape.last().copied().unwrap_or(1024) as usize;
            Ok(l2_normalize(data[..d].to_vec()))
        }
    }

    fn ort_err(e: impl std::fmt::Display) -> crate::Error {
        crate::Error::Io(std::io::Error::other(format!("ort: {e}")))
    }

    #[cfg(feature = "fp32")]
    impl Embedder for Fp32Embedder {
        fn embed_texts(&self, texts: &[String]) -> Result<Vec<Vec<u8>>> {
            texts
                .iter()
                .map(|t| self.encode_one(t).map(|v| to_le_bytes(&v)))
                .collect()
        }

        fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
            self.encode_one(text)
        }

        fn fingerprint(&self) -> EmbeddingFingerprint {
            EmbeddingFingerprint(format!("BAAI/bge-m3|ort-fp32|len={}", self.max_seq_len))
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
        assert!(t.ends_with("abcdefabcd"));
        assert_eq!(t.chars().count(), 2 + 10); // "t\n" + 10 chars
    }
}
