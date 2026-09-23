//! gh-rag MCP server —— LITE(API-only)。
//!
//! 无本地推理引擎:嵌入全部走 OpenAI 兼容 API(默认硅基流动,免费 bge-m3)。
//! 体积 ~8MB,零模型下载,零冷启动加载。需环境变量 GH_RAG_API_KEY。
//! 工具签名与完整版完全一致(DESIGN §3.8)。

use std::sync::Arc;

use gh_rag_core::api_embedder::ApiEmbedder;
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
    store: Arc<std::sync::Mutex<IssueStore>>,
    embedder: Arc<ApiEmbedder>,
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
    eprintln!("[gh-rag-lite] {msg}");
}

impl ServerHandler for GhRag {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            rmcp::model::Implementation::new("gh-rag-lite", env!("CARGO_PKG_VERSION")),
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
                    "Full context pack for one issue: body, labels, top-5 related issues.",
                    json!({"type":"object","required":["repo","number"],"properties":{
                        "repo":{"type":"string"},"number":{"type":"integer"}
                    }}),
                ),
                tool_def(
                    "find_related",
                    "Find issues semantically similar to a given one.",
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
        let store = self.store.clone();
        let embedder = self.embedder.clone();
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
                with_store(&store, |s| {
                    hybrid_search(
                        s,
                        embedder.as_ref(),
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
                with_store(&store, |s| {
                    let m = s
                        .get_issue(&repo, number)
                        .map_err(|e| e.to_string())?
                        .ok_or_else(|| format!("{repo}#{number} not indexed"))?;
                    let _ = s.mark_follow_up(&format!("{repo}#{number}"));
                    let related =
                        find_related(s, &repo, number, 5, None).map_err(|e| e.to_string())?;
                    let relations = s.relations_of(&repo, number).map_err(|e| e.to_string())?;
                    Ok(json!({
                        "repo": m.repo, "number": m.number, "title": m.title,
                        "state": m.state, "labels": m.labels,
                        "comments_count": m.comments_count, "updated_at": m.updated_at,
                        "body": m.body.chars().take(8000).collect::<String>(),
                        // 讨论内容(时间序;总预算 4000 字符防上下文膨胀;含 bot,agent 自行取舍)
                        "comments": m.comments.as_ref().map(|cs| {
                            let mut out = String::new();
                            for c in cs {
                                if out.chars().count() > 4000 { break; }
                                out.push_str(&format!("[- {}] {}\n", c.author,
                                    c.body.chars().take(500).collect::<String>()));
                            }
                            out
                        }).unwrap_or_default(),
                        "related": related.iter().map(|(_, r, n, t, sc)| json!({
                            "repo": r, "number": n, "title": t, "score": sc
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
                with_store(&store, |s| {
                    find_related(s, &repo, number, top_k, None).map_err(|e| e.to_string())
                })
                .map(|hits| {
                    json!(hits
                        .iter()
                        .map(|(_, r, n, t, sc)| json!({
                            "repo": r, "number": n, "title": t, "score": sc,
                        }))
                        .collect::<Vec<_>>())
                })
            }
            "list_repos" => {
                with_store(&store, |s| s.repo_stats().map_err(|e| e.to_string())).map(|stats| {
                    json!(stats
                        .iter()
                        .map(|(r, n, last)| json!({
                            "repo": r, "issues": n, "last_sync": last,
                        }))
                        .collect::<Vec<_>>())
                })
            }
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

fn with_store<T>(
    store: &std::sync::Mutex<IssueStore>,
    f: impl FnOnce(&IssueStore) -> std::result::Result<T, String>,
) -> std::result::Result<T, String> {
    let guard = store
        .lock()
        .map_err(|_| "store lock poisoned".to_string())?;
    f(&guard)
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
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let embedder = ApiEmbedder::from_env()?;
    let store = std::sync::Mutex::new(IssueStore::new(&gh_rag_home().join("index.sqlite"))?);
    log("mode: api-only (siliconflow bge-m3, free) — no local model required");
    log(&format!(
        "index ready ({} repos)",
        with_store_arc(&store, |s| s.repo_stats().map(|v| v.len())).unwrap_or(0)
    ));
    let server = GhRag {
        store: Arc::new(store),
        embedder: Arc::new(embedder),
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

fn with_store_arc<T>(
    store: &std::sync::Mutex<IssueStore>,
    f: impl FnOnce(&IssueStore) -> gh_rag_core::Result<T>,
) -> std::result::Result<T, String> {
    let guard = store.lock().map_err(|_| "lock".to_string())?;
    f(&guard).map_err(|e| e.to_string())
}
