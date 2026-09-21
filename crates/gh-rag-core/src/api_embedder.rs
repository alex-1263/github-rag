//! ApiEmbedder:OpenAI 兼容 /v1/embeddings 接口(默认硅基流动)。
//!
//! 设计动机:API 模式下本地零模型文件——冷启动为零,部署轻量化。
//! 嵌入与查询必须同一实现(向量空间一致性),config 切换后需 rebuild。
//!
//! 配置(环境变量):
//! - GH_RAG_API_BASE:默认 https://api.siliconflow.cn/v1
//! - GH_RAG_API_KEY:必填
//! - GH_RAG_API_MODEL:默认 BAAI/bge-m3

use crate::embedder::{Embedder, EmbeddingFingerprint};
use crate::{Error, Result};

pub struct ApiEmbedder {
    base: String,
    key: String,
    model: String,
    client: ureq::Agent,
    _max_seq_len: usize,
}

#[derive(serde::Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a [String],
}

#[derive(serde::Deserialize)]
struct EmbedResponse {
    #[serde(default)]
    data: Vec<EmbedData>,
}

#[derive(serde::Deserialize)]
struct EmbedData {
    embedding: Vec<f32>,
}

impl ApiEmbedder {
    pub fn from_env() -> Result<Self> {
        let key = std::env::var("GH_RAG_API_KEY").map_err(|_| {
            Error::Io(std::io::Error::other(
                "GH_RAG_API_KEY not set (get one at siliconflow.cn)",
            ))
        })?;
        Ok(Self {
            base: std::env::var("GH_RAG_API_BASE")
                .unwrap_or_else(|_| "https://api.siliconflow.cn/v1".into()),
            key,
            model: std::env::var("GH_RAG_API_MODEL").unwrap_or_else(|_| "BAAI/bge-m3".into()),
            client: ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(60))
                .build(),
            _max_seq_len: 512,
        })
    }

    fn call(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        // 分批:单请求最多 64 条(服务端限制保守值)
        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(64) {
            let body = EmbedRequest {
                model: &self.model,
                input: chunk,
            };
            let resp = self
                .client
                .post(format!("{}/embeddings", self.base).as_str())
                .set("Authorization", &format!("Bearer {}", self.key))
                .send_json(&body);
            let resp = match resp {
                Ok(r) => r,
                Err(ureq::Error::Status(code, r)) => {
                    // 带上平台原始错误(如 30014 Token is invalid),不再只给裸 status code
                    let body = r.into_string().unwrap_or_default();
                    let msg: Option<String> = serde_json::from_str::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|v| {
                            v.get("message")
                                .and_then(|m| m.as_str().map(|s| s.to_string()))
                        });
                    return Err(Error::Io(std::io::Error::other(format!(
                        "api {}: {}",
                        code,
                        msg.unwrap_or(body.chars().take(120).collect())
                    ))));
                }
                Err(e) => {
                    return Err(Error::Io(std::io::Error::other(format!("api: {e}"))));
                }
            };
            let resp: EmbedResponse = resp
                .into_json()
                .map_err(|e| Error::Io(std::io::Error::other(format!("api body: {e}"))))?;
            if resp.data.len() != chunk.len() {
                return Err(Error::Io(std::io::Error::other(format!(
                    "api returned {} embeddings for {} inputs",
                    resp.data.len(),
                    chunk.len()
                ))));
            }
            out.extend(resp.data.into_iter().map(|d| l2(d.embedding)));
        }
        Ok(out)
    }
}

fn l2(mut v: Vec<f32>) -> Vec<f32> {
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

impl Embedder for ApiEmbedder {
    fn embed_texts(&self, texts: &[String]) -> Result<Vec<Vec<u8>>> {
        Ok(self
            .call(texts)?
            .into_iter()
            .map(|v| to_le_bytes(&v))
            .collect())
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        Ok(self
            .call(&[text.to_string()])?
            .into_iter()
            .next()
            .unwrap_or_default())
    }

    fn fingerprint(&self) -> EmbeddingFingerprint {
        EmbeddingFingerprint(format!("{}|api|len=512", self.model))
    }
}
