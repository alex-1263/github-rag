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
    dimensions: Option<u32>,
    batch: usize,
    client: ureq::Agent,
}

#[derive(serde::Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    dimensions: Option<u32>,
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
        let cfg = crate::config::resolve()?;
        if cfg.key.is_empty() {
            return Err(Error::Io(std::io::Error::other(
                "GH_RAG_API_KEY not set — set the environment variable, or fill in embedding.api_key in config.toml (get one for free at siliconflow.cn)",
            )));
        }
        Ok(Self {
            base: cfg.base,
            key: cfg.key,
            model: cfg.model,
            dimensions: cfg.dimensions,
            batch: cfg.batch_size,
            client: ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(60))
                .build(),
        })
    }

    fn call(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        // 分批:单请求最多 64 条(服务端限制保守值)
        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(self.batch.max(1)) {
            let body = EmbedRequest {
                model: &self.model,
                input: chunk,
                dimensions: self.dimensions,
            };
            let resp = self.send_with_retry(&body)?;
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

    /// 发送 + 429 指数退避(官方推荐姿势:1s/2s/4s,最多 3 次重试)。
    fn send_with_retry(&self, body: &EmbedRequest<'_>) -> Result<EmbedResponse> {
        let mut backoff = 0u32;
        loop {
            let resp = self
                .client
                .post(format!("{}/embeddings", self.base).as_str())
                .set("Authorization", &format!("Bearer {}", self.key))
                .send_json(body);
            match resp {
                Ok(r) => {
                    return r
                        .into_json()
                        .map_err(|e| Error::Io(std::io::Error::other(format!("api body: {e}"))))
                }
                Err(ureq::Error::Status(429, r)) => {
                    let _ = r.into_string();
                    if backoff >= 3 {
                        return Err(Error::Io(std::io::Error::other(
                            "api 429: 重试 3 次后仍限流,稍后再跑 sync",
                        )));
                    }
                    let secs = 1u64 << backoff; // 1s, 2s, 4s
                    eprintln!("[gh-rag] api 429,退避 {secs}s(第 {} 次)", backoff + 1);
                    std::thread::sleep(std::time::Duration::from_secs(secs));
                    backoff += 1;
                }
                Err(ureq::Error::Status(code, r)) => {
                    // 带上平台原始错误(如 30014 Token is invalid)
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
            }
        }
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
        // 维度进 model 段:不同维度 = 不同向量空间,ensure 按空间拦截
        let model = match self.dimensions {
            Some(d) => format!("{}[dim={}]", self.model, d),
            None => self.model.clone(),
        };
        EmbeddingFingerprint(format!("{}|api|len=512", model))
    }
}
