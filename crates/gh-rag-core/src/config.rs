//! 嵌入 API 配置:`~/.gh-rag/config.toml [embedding]` + 环境变量覆盖 + 内置 provider 预设。
//!
//! 优先级(高 → 低):
//! 1. 环境变量 GH_RAG_API_BASE / GH_RAG_API_MODEL / GH_RAG_API_KEY(脚本与 CI 场景)
//! 2. config.toml `[embedding]` 段(provider 预设名 + 覆盖项)
//! 3. 内置 provider 预设默认值
//!
//! 预设清单(接入细节见 docs/free-models.md):
//! - siliconflow:国内直连,免费 bge-m3(默认)
//! - ollama:本机 127.0.0.1:11434,模型名 bge-m3
//! - openai / jina:海外,需网络
//! - custom:自建端点(vLLM / TEI / 网关),必须显式 base_url

use crate::{Error, Result};

/// 内置 provider 预设。
pub const PRESETS: &[(&str, &str, &str)] = &[
    // (provider, base_url, model)
    (
        "siliconflow",
        "https://api.siliconflow.cn/v1",
        "BAAI/bge-m3",
    ),
    (
        "aliyun",
        "https://dashscope.aliyuncs.com/compatible-mode/v1",
        "qwen3.7-text-embedding-flash",
    ),
    ("ollama", "http://127.0.0.1:11434/v1", "bge-m3"),
    (
        "openai",
        "https://api.openai.com/v1",
        "text-embedding-3-small",
    ),
    ("jina", "https://api.jina.ai/v1", "jina-embeddings-v3"),
];

pub const DEFAULT_PROVIDER: &str = "siliconflow";

/// 解析后的嵌入端点三元组。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEmbedding {
    pub base: String,
    pub key: String,
    pub model: String,
    /// 自定义输出维度(仅部分模型支持,如 qwen3.7 系 256~2560);None = 模型默认
    pub dimensions: Option<u32>,
    /// 单请求批量上限(硅基流动 64,百炼 16)
    pub batch_size: usize,
}

/// config.toml `[embedding]` 段的(部分)形状;未知字段忽略,向后兼容。
#[derive(Debug, Default, serde::Deserialize)]
struct EmbeddingSection {
    provider: Option<String>,
    api_key: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    dimensions: Option<u32>,
    batch_size: Option<usize>,
}

/// 端点默认批量:硅基流动 64;其余(百炼等)16。
fn default_batch_for(base: &str) -> usize {
    if base.contains("siliconflow") {
        64
    } else {
        16
    }
}

pub fn gh_rag_home() -> std::path::PathBuf {
    if let Ok(h) = std::env::var("GH_RAG_HOME") {
        return h.into();
    }
    #[cfg(windows)]
    let key = "USERPROFILE";
    #[cfg(not(windows))]
    let key = "HOME";
    std::env::var(key)
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
        .join(".gh-rag")
}

/// 解析当前环境的嵌入配置(环境变量 > config > 预设默认)。
pub fn resolve() -> Result<ResolvedEmbedding> {
    resolve_with_home(gh_rag_home())
}

/// config.toml 顶层 `token`(GitHub);空/缺省回落 `gh auth token`。
pub fn github_token() -> Result<Option<String>> {
    #[derive(Default, serde::Deserialize)]
    struct Top {
        token: Option<String>,
    }
    let t: Top = std::fs::read_to_string(gh_rag_home().join("config.toml"))
        .ok()
        .and_then(|raw| toml::from_str(&raw).ok())
        .unwrap_or_default();
    Ok(t.token.filter(|s| !s.trim().is_empty()))
}
/// config 顶层 `repos` 列表。
pub fn repos() -> Result<Option<Vec<String>>> {
    #[derive(Default, serde::Deserialize)]
    struct Top {
        repos: Option<Vec<String>>,
    }
    let t: Top = std::fs::read_to_string(gh_rag_home().join("config.toml"))
        .ok()
        .and_then(|raw| toml::from_str(&raw).ok())
        .unwrap_or_default();
    Ok(t.repos.filter(|v| !v.is_empty()))
}

/// config.toml `[eval]` 段解析结果(回落已展开)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalConfig {
    pub judge_model: String,
    pub base_url: String,
    pub api_key: String,
    pub days: u32,
    pub top_k: usize,
}

#[derive(Debug, Default, serde::Deserialize)]
struct EvalSection {
    judge_model: Option<String>,
    base_url: Option<String>,
    api_key: Option<String>,
    days: Option<u32>,
    top_k: Option<usize>,
}

const DEFAULT_JUDGE_MODEL: &str = "qwen-flash";

/// `[eval]` 段解析;base_url/api_key 缺省回落嵌入配置(含 GH_RAG_API_KEY 环境变量链)。
pub fn eval_config() -> Result<EvalConfig> {
    eval_config_with_home(gh_rag_home())
}

pub fn eval_config_with_home(home: std::path::PathBuf) -> Result<EvalConfig> {
    let section: EvalSection = std::fs::read_to_string(home.join("config.toml"))
        .ok()
        .and_then(|raw| toml::from_str::<toml::Value>(&raw).ok())
        .and_then(|v| v.get("eval").cloned())
        .and_then(|s| s.try_into().ok())
        .unwrap_or_default();
    // 回落来源:嵌入解析结果(环境变量 > [embedding] > 预设)
    let emb = resolve_with_home(home).unwrap_or_else(|_| ResolvedEmbedding {
        base: String::new(),
        key: String::new(),
        model: String::new(),
        dimensions: None,
        batch_size: 16,
    });
    Ok(EvalConfig {
        judge_model: section
            .judge_model
            .unwrap_or_else(|| DEFAULT_JUDGE_MODEL.to_string()),
        base_url: section.base_url.unwrap_or(emb.base),
        api_key: section
            .api_key
            .or_else(|| {
                if emb.key.is_empty() {
                    None
                } else {
                    Some(emb.key.clone())
                }
            })
            .unwrap_or_default(),
        days: section.days.unwrap_or(7),
        top_k: section.top_k.unwrap_or(5),
    })
}

/// 检索参数 [retrieval](vec_top/fts_top/rrf_k/top_k/snippet_chars;缺省与 SearchParams::default 一致)。
pub fn search_params() -> Result<crate::retrieve::SearchParams> {
    #[derive(Default, serde::Deserialize)]
    struct R {
        vec_top: Option<usize>,
        fts_top: Option<usize>,
        rrf_k: Option<usize>,
        snippet_chars: Option<usize>,
    }
    let r: R = std::fs::read_to_string(gh_rag_home().join("config.toml"))
        .ok()
        .and_then(|raw| toml::from_str::<toml::Value>(&raw).ok())
        .and_then(|v| v.get("retrieval").cloned())
        .and_then(|s| s.try_into().ok())
        .unwrap_or_default();
    let d = crate::retrieve::SearchParams::default();
    Ok(crate::retrieve::SearchParams {
        vec_top: r.vec_top.unwrap_or(d.vec_top),
        fts_top: r.fts_top.unwrap_or(d.fts_top),
        rrf_k: r.rrf_k.unwrap_or(d.rrf_k),
        snippet_chars: r.snippet_chars.unwrap_or(d.snippet_chars),
    })
}

/// 文本组装参数 [retrieval] title_repeats / body_max_chars(与检索一致,建库必须同参)。
pub fn text_params() -> Result<(usize, usize)> {
    #[derive(Default, serde::Deserialize)]
    struct R {
        title_repeats: Option<usize>,
        body_max_chars: Option<usize>,
    }
    let r: R = std::fs::read_to_string(gh_rag_home().join("config.toml"))
        .ok()
        .and_then(|raw| toml::from_str::<toml::Value>(&raw).ok())
        .and_then(|v| v.get("retrieval").cloned())
        .and_then(|s| s.try_into().ok())
        .unwrap_or_default();
    Ok((
        r.title_repeats.unwrap_or(2),
        r.body_max_chars.unwrap_or(2000),
    ))
}

/// 嵌入批间节流 [embedding] batch_interval_ms(默认 4000,免费档保守值)。
pub fn batch_interval_ms() -> Result<u64> {
    #[derive(Default, serde::Deserialize)]
    struct E {
        batch_interval_ms: Option<u64>,
    }
    let e: E = std::fs::read_to_string(gh_rag_home().join("config.toml"))
        .ok()
        .and_then(|raw| toml::from_str::<toml::Value>(&raw).ok())
        .and_then(|v| v.get("embedding").cloned())
        .and_then(|s| s.try_into().ok())
        .unwrap_or_default();
    Ok(e.batch_interval_ms.unwrap_or(4000))
}

/// 可注入 home 的解析核心(测试用);环境变量照常读取。
pub fn resolve_with_home(home: std::path::PathBuf) -> Result<ResolvedEmbedding> {
    let section = std::fs::read_to_string(home.join("config.toml"))
        .ok()
        .and_then(|raw| toml::from_str::<toml::Value>(&raw).ok())
        .and_then(|v| v.get("embedding").cloned())
        .and_then(|e| e.try_into::<EmbeddingSection>().ok())
        .unwrap_or_default();

    let provider = std::env::var("GH_RAG_EMBEDDER")
        .ok()
        .filter(|v| v != "api") // 历史值 api 视为默认 provider 流程
        .or(section.provider.clone())
        .unwrap_or_else(|| DEFAULT_PROVIDER.to_string());

    let (preset_base, preset_model) = match PRESETS.iter().find(|(p, _, _)| *p == provider) {
        Some((_, b, m)) => ((*b).to_string(), (*m).to_string()),
        None if provider == "custom" => (String::new(), String::new()),
        None if provider == "local" => {
            return Err(Error::Config(
                "GH_RAG_EMBEDDER=local 已退役(本地 ONNX 推理 2026-09 移除);本机推理请用 provider = \"ollama\""
                    .to_string(),
            ));
        }
        None => {
            return Err(Error::Config(format!(
                "未知 embedding provider `{provider}`:可选 {} 或 custom",
                PRESETS
                    .iter()
                    .map(|(p, _, _)| *p)
                    .collect::<Vec<_>>()
                    .join("/")
            )));
        }
    };

    let base = std::env::var("GH_RAG_API_BASE")
        .ok()
        .or(section.base_url.clone())
        .unwrap_or(preset_base);
    if base.is_empty() {
        return Err(Error::Config(
            "provider=custom 必须显式 base_url(或环境变量 GH_RAG_API_BASE)".to_string(),
        ));
    }

    let model = std::env::var("GH_RAG_API_MODEL")
        .ok()
        .or(section.model.clone())
        .unwrap_or(preset_model);
    if model.is_empty() {
        return Err(Error::Config(
            "provider=custom 必须显式 model(或环境变量 GH_RAG_API_MODEL)".to_string(),
        ));
    }

    let key = std::env::var("GH_RAG_API_KEY")
        .ok()
        .or(section.api_key.clone())
        .unwrap_or_default();

    let batch_size = section
        .batch_size
        .unwrap_or_else(|| default_batch_for(&base));
    Ok(ResolvedEmbedding {
        base,
        key,
        model,
        dimensions: section.dimensions,
        batch_size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home_with(tag: &str, config: Option<&str>) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("gh-rag-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(c) = config {
            std::fs::write(dir.join("config.toml"), c).unwrap();
        }
        dir
    }

    fn with_home(tag: &str, config: Option<&str>) -> Result<ResolvedEmbedding> {
        resolve_with_home(home_with(tag, config))
    }

    #[test]
    fn eval_section_defaults() {
        let c = eval_config_with_home(home_with(
            "eval-default",
            Some("[embedding]\nbase_url = \"http://e:1/v1\"\napi_key = \"ek\"\n"),
        ))
        .unwrap();
        assert_eq!(c.judge_model, "qwen-flash");
        assert_eq!(c.days, 7);
        assert_eq!(c.top_k, 5);
        // 缺省回落 [embedding]
        assert_eq!(c.base_url, "http://e:1/v1");
        assert_eq!(c.api_key, "ek");
    }

    #[test]
    fn eval_section_overrides() {
        let c = eval_config_with_home(home_with(
            "eval-override",
            Some(
                "[embedding]\nbase_url = \"http://e:1/v1\"\napi_key = \"ek\"\n\
                 [eval]\njudge_model = \"qwen-plus\"\nbase_url = \"http://j:2/v1\"\n\
                 api_key = \"jk\"\ndays = 30\ntop_k = 10\n",
            ),
        ))
        .unwrap();
        assert_eq!(c.judge_model, "qwen-plus");
        assert_eq!(c.base_url, "http://j:2/v1");
        assert_eq!(c.api_key, "jk");
        assert_eq!(c.days, 30);
        assert_eq!(c.top_k, 10);
    }

    #[test]
    fn eval_missing_embedding_falls_back_to_empty_key() {
        let c = eval_config_with_home(home_with("eval-noemb", None)).unwrap();
        assert_eq!(c.judge_model, "qwen-flash");
        assert_eq!(c.days, 7);
        assert_eq!(c.top_k, 5);
        assert!(c.api_key.is_empty(), "无任何来源时 key 为空,由调用方报错");
    }

    #[test]
    fn presets_cover_expected_providers() {
        assert!(PRESETS.len() >= 4);
        assert!(PRESETS.iter().any(|(p, _, _)| *p == "siliconflow"));
        assert!(PRESETS.iter().any(|(p, _, _)| *p == "ollama"));
    }

    #[test]
    fn missing_config_falls_back_to_siliconflow() {
        // 无 config → 默认 siliconflow(若环境变量覆盖了 base,则仅验证可解析)
        let r = with_home("empty", None).unwrap();
        assert!(
            r.base.contains("siliconflow") || std::env::var("GH_RAG_API_BASE").is_ok(),
            "base={}",
            r.base
        );
    }

    #[test]
    fn custom_requires_base_url() {
        let r = with_home(
            "custom-nobase",
            Some("[embedding]\nprovider = \"custom\"\n"),
        );
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("base_url"));
    }

    #[test]
    fn custom_with_base_resolves() {
        let r = with_home(
            "custom-ok",
            Some("[embedding]\nprovider = \"custom\"\nbase_url = \"http://10.0.0.1:8080/v1\"\nmodel = \"m\"\n"),
        )
        .unwrap();
        assert_eq!(r.base, "http://10.0.0.1:8080/v1");
        assert_eq!(r.model, "m");
    }

    #[test]
    fn unknown_provider_rejected() {
        let r = with_home("bad", Some("[embedding]\nprovider = \"nope\"\n"));
        assert!(r.is_err());
    }

    #[test]
    fn provider_preset_applies_base_and_model() {
        let r = with_home(
            "mix",
            Some("[embedding]\nprovider = \"ollama\"\napi_key = \"k\"\n"),
        )
        .unwrap();
        assert_eq!(r.base, "http://127.0.0.1:11434/v1");
        assert_eq!(r.model, "bge-m3");
        assert_eq!(r.key, "k");
    }

    #[test]
    fn legacy_fields_are_tolerated() {
        let r = with_home(
            "legacy",
            Some("[embedding]\nmodel = \"BAAI/bge-m3\"\nhf_mirror = true\nbatch_size = 64\n"),
        )
        .unwrap();
        assert!(r.base.contains("siliconflow"));
        assert_eq!(r.model, "BAAI/bge-m3");
    }
}
