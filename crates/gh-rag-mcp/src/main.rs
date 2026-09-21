//! gh-rag MCP server(rmcp 3.x):四工具,签名与 Python 版一致(DESIGN §3.8 冻结契约)。
//!
//! Embedder:GH_RAG_EMBEDDER=api 走硅基流动(需 GH_RAG_API_KEY),
//! 默认本地 fp32 ONNX(与 Python 建库空间逐位一致,黄金测试背书)。

use std::sync::Arc;

use gh_rag_core::embedder::onnx::Fp32Embedder;
use gh_rag_core::embedder::Embedder;
use gh_rag_core::retrieve::{find_related, hybrid_search, SearchFilter, SearchParams};
use gh_rag_core::store::IssueStore;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, ContentBlock, ListToolsResult, ServerCapabilities,
    ServerConfig, Tool,
};
use rmcp::service::{RequestContext, RoleServer, ServiceExt};
use rmcp::{ErrorData as McpError, ServerHandler};
use serde_json::json;

#[derive(Clone)]
struct GhRag {
    core: Arc<Core>,
}

struct Core {
    store: std::sync::Mutex<IssueStore>,
    embedder: Box<dyn Embedder + Send + Sync>,
}

impl Core {
    fn with_store<T>(
        &self,
        f: impl FnOnce(&IssueStore) -> std::result::Result<T, String>,
    ) -> std::result::Result<T, String> {
        let guard = self
            .store
            .lock()
            .map_err(|_| "store lock poisoned".to_string())?;
        f(&guard)
    }

    fn load() -> anyhow::Result<Self> {
        let store = IssueStore::new(&gh_rag_home().join("index.sqlite"))?;
        let embedder: Box<dyn Embedder + Send + Sync> = match std::env::var("GH_RAG_EMBEDDER")
            .unwrap_or_default()
            .as_str()
        {
            "" | "api" => {
                log("embedder: api (BAAI/bge-m3 via siliconflow) — default, zero model download");
                Box::new(gh_rag_core::api_embedder::ApiEmbedder::from_env().map_err(|e| {
                    anyhow::anyhow!(
                        "{e}\n  默认嵌入走 API,需要 GH_RAG_API_KEY。\n  \
                         两条路:① 设置 GH_RAG_API_KEY(硅基流动免费档即可) \
                         ② 设 GH_RAG_EMBEDDER=local 走本地推理(需 ~/.gh-rag/models/bge-m3/)"
                    )
                })?)
            }
            "local" => {
                log("embedder: local fp32 onnx");
                Box::new(Fp32Embedder::new(512)?)
            }
            other => anyhow::bail!(
                "GH_RAG_EMBEDDER={other} 无效:可选 api(默认)/ local"
            ),
        };
        Ok(Self {
            store: std::sync::Mutex::new(store),
            embedder,
        })
    }
}

fn gh_rag_home() -> std::path::PathBuf {
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

fn log(msg: &str) {
    // stdio 模式下 stdout 属于协议通道,诊断走 stderr
    eprintln!("[gh-rag] {msg}");
}

impl ServerHandler for GhRag {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            rmcp::model::Implementation::new("gh-rag", env!("CARGO_PKG_VERSION")),
        )
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult {
            tools: vec![
                tool_def(
                    "search_issues",
                    "Semantically search GitHub issues across the user's indexed repositories. \
                     Use BEFORE starting work on a feature or bug: check whether someone already \
                     reported it, find prior discussions. Accepts natural-language queries.",
                    json!({"type":"object","required":["query"],"properties":{
                        "query":{"type":"string"},
                        "repos":{"type":"array","items":{"type":"string"}},
                        "state":{"type":"string","enum":["open","closed","all"]},
                        "labels":{"type":"array","items":{"type":"string"}},
                        "top_k":{"type":"integer","default":5}
                    }}),
                ),
                tool_def(
                    "get_issue_context",
                    "Full context pack for one issue: body, labels, top-5 related issues. \
                     Call after search_issues when digging into a specific hit.",
                    json!({"type":"object","required":["repo","number"],"properties":{
                        "repo":{"type":"string"},"number":{"type":"integer"}
                    }}),
                ),
                tool_def(
                    "find_related",
                    "Find issues semantically similar to a given one. Use for duplicate \
                     detection or broadening a narrow result.",
                    json!({"type":"object","required":["repo","number"],"properties":{
                        "repo":{"type":"string"},"number":{"type":"integer"},
                        "top_k":{"type":"integer","default":10}
                    }}),
                ),
                tool_def(
                    "list_repos",
                    "List indexed repositories with issue counts and last-sync time.",
                    json!({"type":"object","properties":{}}),
                ),
            ],
            next_cursor: None,
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let name = request.name.as_ref();
        let args = request.arguments.unwrap_or_default();
        let core = &self.core;
        let res: std::result::Result<serde_json::Value, String> = match name {
            "search_issues" => {
                let Some(query) = str_arg(&args, "query") else {
                    return bad_request("query required");
                };
                let filter = SearchFilter {
                    repos: opt_str_vec(&args, "repos"),
                    state: opt_str(&args, "state"),
                    labels: opt_str_vec(&args, "labels"),
                };
                let top_k = args.get("top_k").and_then(|v| v.as_i64()).unwrap_or(5) as usize;
                core.with_store(|store| {
                    hybrid_search(
                        store,
                        core.embedder.as_ref(),
                        &query,
                        &filter,
                        top_k,
                        &SearchParams::default(),
                    )
                    .map_err(|e| e.to_string())
                })
                .map(|hits| {
                    json!(hits
                        .iter()
                        .map(|h| json!({
                            "repo": h.repo, "number": h.number, "title": h.title,
                            "state": h.state, "snippet": h.snippet,
                            "score": h.score, "source": h.source,
                        }))
                        .collect::<Vec<_>>())
                })
            }
            "get_issue_context" => {
                let Some(repo) = str_arg(&args, "repo") else {
                    return bad_request("repo required");
                };
                let Some(number) = args.get("number").and_then(|v| v.as_i64()) else {
                    return bad_request("number required");
                };
                core.with_store(|store| {
                    let m = store
                        .get_issue(&repo, number)
                        .map_err(|e| e.to_string())?
                        .ok_or_else(|| format!("{repo}#{number} not indexed"))?;
                    let _ = store.mark_follow_up(&format!("{repo}#{number}"));
                    let related =
                        find_related(store, &repo, number, 5, None).map_err(|e| e.to_string())?;
                    let relations = store
                        .relations_of(&repo, number)
                        .map_err(|e| e.to_string())?;
                    Ok(json!({
                        "repo": m.repo, "number": m.number, "title": m.title,
                        "state": m.state, "labels": m.labels,
                        "comments_count": m.comments_count, "updated_at": m.updated_at,
                        "body": truncate(&m.body, 8000),
                        "related": related.iter().map(|(_, r, n, t, s)| json!({
                            "repo": r, "number": n, "title": t, "score": s
                        })).collect::<Vec<_>>(),
                        "relations": relations,
                    }))
                })
            }
            "find_related" => {
                let Some(repo) = str_arg(&args, "repo") else {
                    return bad_request("repo required");
                };
                let Some(number) = args.get("number").and_then(|v| v.as_i64()) else {
                    return bad_request("number required");
                };
                let top_k = args.get("top_k").and_then(|v| v.as_i64()).unwrap_or(10) as usize;
                core.with_store(|store| {
                    find_related(store, &repo, number, top_k, None).map_err(|e| e.to_string())
                })
                .map(|hits| {
                    json!(hits
                        .iter()
                        .map(|(_, r, n, t, s)| json!({
                            "repo": r, "number": n, "title": t, "score": s,
                        }))
                        .collect::<Vec<_>>())
                })
            }
            "list_repos" => core
                .with_store(|store| store.repo_stats().map_err(|e| e.to_string()))
                .map(|stats| {
                    json!(stats
                        .iter()
                        .map(|(r, n, last)| json!({
                            "repo": r, "issues": n, "last_sync": last,
                        }))
                        .collect::<Vec<_>>())
                }),
            _ => Err(format!("unknown tool: {name}")),
        };
        let result = match res {
            Ok(v) => rmcp::model::CallToolResult::success(vec![ContentBlock::text(
                serde_json::to_string(&v).unwrap_or_default(),
            )]),
            Err(e) => rmcp::model::CallToolResult::error(vec![ContentBlock::text(e)]),
        };
        Ok(result.into())
    }
}

fn bad_request(msg: &str) -> Result<CallToolResponse, McpError> {
    Err(McpError::invalid_params(msg.to_string(), None))
}

fn tool_def(name: &str, desc: &str, schema: serde_json::Value) -> Tool {
    let schema_map = schema.as_object().cloned().unwrap_or_default();
    Tool::new(
        name.to_string(),
        desc.to_string(),
        std::sync::Arc::new(schema_map),
    )
}

fn str_arg(args: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    args.get(key)?.as_str().map(|s| s.to_string())
}

fn opt_str(args: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn opt_str_vec(
    args: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<Vec<String>> {
    args.get(key)?.as_array().map(|a| {
        a.iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect()
    })
}

fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let core = Core::load()?;
    log(&format!(
        "index ready ({} repos, fp={:?})",
        core.with_store(|s| s.repo_stats().map(|v| v.len()).map_err(|e| e.to_string()))
            .unwrap_or(0),
        core.with_store(|s| Ok(s.manifest_get("embedding_fp").ok().flatten()))
            .ok()
            .flatten()
    ));
    let server = GhRag {
        core: Arc::new(core),
    };
    let service = server
        .serve(rmcp::transport::io::stdio())
        .await
        .map_err(|e| anyhow::anyhow!("serve: {e}"))?;
    log("mcp serving on stdio");
    service
        .waiting()
        .await
        .map_err(|e| anyhow::anyhow!("wait: {e}"))?;
    Ok(())
}
