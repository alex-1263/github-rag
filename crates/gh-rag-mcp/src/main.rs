//! gh-rag MCP server —— LITE(API-only)。
//!
//! 无本地推理引擎:嵌入全部走 OpenAI 兼容 API(默认硅基流动,免费 bge-m3)。
//! 体积 ~8MB,零模型下载,零冷启动加载。嵌入懒初始化:无 GH_RAG_API_KEY 也能启动,
//! 首次语义检索时才构造(失败报该次工具错误,不退出进程)。
//! 工具签名与完整版完全一致(AGENTS 冻结契约)。

use std::sync::Arc;

use gh_rag_core::api_embedder::ApiEmbedder;
use gh_rag_core::retrieve::{find_related, hybrid_search_with_query, SearchFilter};
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
    /// 懒初始化:启动不要求 GH_RAG_API_KEY,首次嵌入调用才构造;
    /// 构造失败只报该次工具错误,进程不退出(list_repos / get_issue_context 无需 key)。
    embedder: Arc<std::sync::Mutex<Option<ApiEmbedder>>>,
    /// 启动时指纹防线:索引 embedding_fp 的向量空间(model|dim|len)与当前 config 期望
    /// 不一致时的告警文案;search_issues 直接报错提示重建(其余工具照常)。
    space_error: Option<String>,
}

/// 当前 config 期望的向量空间(与 ApiEmbedder::fingerprint 同一构造:model[dim]|len)。
fn expected_space() -> Option<(String, String)> {
    let cfg = gh_rag_core::config::resolve().ok()?;
    let model = match cfg.dimensions {
        Some(d) => format!("{}[dim={}]", cfg.model, d),
        None => cfg.model.clone(),
    };
    Some(gh_rag_core::store::fingerprint_space(&format!(
        "{model}|api|len=512"
    )))
}

/// 启动指纹防线:不匹配返回告警文案(仅 model/dim 空间比对,宽松于 sync 侧 full 指纹)。
fn space_warning(store: &IssueStore) -> Option<String> {
    let db_fp = store.manifest_get("embedding_fp").ok()??;
    let expected = expected_space()?;
    if gh_rag_core::store::fingerprint_space(&db_fp) != expected {
        return Some(format!(
            "嵌入空间不匹配:索引按 [{db_fp}] 构建,当前配置期望 {expected:?} — 检索结果不可信,请重建索引(gh-rag sync)"
        ));
    }
    None
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

impl GhRag {
    /// 懒构造 embedder(首次调用才读配置/校验 key)并执行 embed_query。
    /// 网络调用不持库锁——调用方拿到向量后再进 with_store。
    fn embed_query(&self, query: &str) -> std::result::Result<Vec<f32>, String> {
        use gh_rag_core::embedder::Embedder as _;
        let mut slot = self
            .embedder
            .lock()
            .map_err(|_| "embedder lock poisoned".to_string())?;
        if slot.is_none() {
            *slot = Some(ApiEmbedder::from_env().map_err(|e| {
                format!("嵌入端点不可用:{e}(search_issues/find_related 语义腿需要;list_repos/get_issue_context 不需要)")
            })?);
        }
        slot.as_ref()
            .expect("slot just filled")
            .embed_query(query)
            .map_err(|e| e.to_string())
    }
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
                // 指纹防线:索引空间与当前配置不符 → 不做检索,提示重建
                if let Some(w) = &self.space_error {
                    return tool_err(w);
                }
                // 嵌入先行(网络调用不持库锁),入库后取锁做召回
                let res = self.embed_query(&query).and_then(|q| {
                    with_store(&store, |s| {
                        hybrid_search_with_query(
                            s,
                            &q,
                            &query,
                            &filter,
                            top_k,
                            // 检索参数从 config [retrieval] 读(AGENTS 纪律),解析失败回落默认
                            &gh_rag_core::config::search_params().unwrap_or_default(),
                            "search_issues",
                        )
                        .map_err(|e| e.to_string())
                    })
                });
                res.map(|hits| {
                    json!(hits
                        .iter()
                        .map(|h| json!({
                            "repo": h.repo, "number": h.number, "kind": h.kind, "title": h.title,
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
                    let _ = s.mark_follow_up(&repo, number);
                    let related =
                        find_related(s, &repo, number, 5, None).map_err(|e| e.to_string())?;
                    let relations = relations_payload(
                        &s.relations_of(&repo, number).map_err(|e| e.to_string())?,
                        &s.relations_reverse(&repo, number)
                            .map_err(|e| e.to_string())?,
                    );
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
                with_store(&store, |s| {
                    let stats = s.repo_stats().map_err(|e| e.to_string())?;
                    let facets = s.label_facets().map_err(|e| e.to_string())?;
                    Ok((stats, facets))
                })
                .map(|(stats, facets)| {
                    // facets 已按 repo、count 降序,按 repo 归组即得各仓标签列表
                    let mut labels_by_repo: std::collections::HashMap<
                        &str,
                        Vec<serde_json::Value>,
                    > = std::collections::HashMap::new();
                    for (r, label, count) in &facets {
                        labels_by_repo
                            .entry(r)
                            .or_default()
                            .push(json!({ "name": label, "count": count }));
                    }
                    json!(stats
                        .iter()
                        .map(|(r, n, last)| json!({
                            "repo": r, "issues": n, "last_sync": last,
                            "labels": labels_by_repo.get(r.as_str()).cloned().unwrap_or_default(),
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

/// 工具层错误文本(不 crash 进程,调用方可见)。
fn tool_err(msg: &str) -> Result<CallToolResponse, McpError> {
    Ok(rmcp::model::CallToolResult::error(vec![ContentBlock::text(msg.to_string())]).into())
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
    let store = std::sync::Mutex::new(IssueStore::new(&gh_rag_home().join("index.sqlite"))?);
    // 启动指纹防线(读路径):manifest embedding_fp 与当前 config 期望空间不一致 → 醒目警告
    let space_error = with_store_arc(&store, |s| Ok::<_, gh_rag_core::Error>(space_warning(s)))
        .unwrap_or_else(|e| {
            eprintln!("[gh-rag-lite] 指纹防线检查失败:{e}");
            None
        });
    if let Some(w) = &space_error {
        log(&format!("⚠ {w}"));
    }
    log("mode: api-only (默认 siliconflow bge-m3) — 嵌入懒初始化:无 GH_RAG_API_KEY 也能起,list_repos 可用;首次语义检索时才需要 key");
    log(&format!(
        "index ready ({} repos)",
        with_store_arc(&store, |s| s.repo_stats().map(|v| v.len())).unwrap_or(0)
    ));
    let server = GhRag {
        store: Arc::new(store),
        embedder: Arc::new(std::sync::Mutex::new(None)),
        space_error,
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

/// M2.5:get_issue_context 的 relations 升级体。
/// 正向 rows = relations_of,反向 rows = relations_reverse;fixes 的反向转义为 fixed_by。
/// 分类键 fixes/closes/fixed_by/refs,空类返回空数组(非 null);每项 {repo, number}。
fn relations_payload(
    fwd: &[(String, String, i64)],
    rev: &[(String, String, i64)],
) -> serde_json::Value {
    let items = |rows: &[(String, String, i64)], want: &str| {
        json!(rows
            .iter()
            .filter(|(k, _, _)| k == want)
            .map(|(_, r, n)| json!({"repo": r, "number": n}))
            .collect::<Vec<_>>())
    };
    let mut v = serde_json::Map::new();
    for k in ["fixes", "closes"] {
        v.insert(k.into(), items(fwd, k));
    }
    // fixes 的反向 = fixed_by;refs 反向一般无人消费,不透出
    v.insert(
        "fixed_by".into(),
        items(
            &rev.iter()
                .filter(|(k, _, _)| k == "fixes")
                .cloned()
                .collect::<Vec<_>>(),
            "fixes",
        ),
    );
    v.insert("refs".into(), items(fwd, "refs"));
    json!(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relations_payload_classifies_and_defaults_empty() {
        let fwd = vec![
            ("fixes".to_string(), "o/r".to_string(), 3),
            ("refs".to_string(), "x/y".to_string(), 9),
        ];
        let rev = vec![("fixes".to_string(), "t/a".to_string(), 7)];
        let v = relations_payload(&fwd, &rev);
        assert_eq!(v["fixes"], json!([{"repo": "o/r", "number": 3}]));
        assert_eq!(v["fixed_by"], json!([{"repo": "t/a", "number": 7}]));
        assert_eq!(v["refs"], json!([{"repo": "x/y", "number": 9}]));
        assert_eq!(v["closes"], json!([]), "空类为数组非 null");
    }
}
