# gh-rag

GitHub issue 的语义记忆层:跨仓库混合检索(向量 + BM25 + RRF),MCP server 形态,供 Claude Code / Copilot / OMP 等 agent 消费。设计文档见 [DESIGN.md](DESIGN.md)。

**形态:纯 Rust 单二进制 + 纯 API 嵌入**(2026-09 架构收敛:本地 ONNX 推理与 Python 验证版已退役,exe 仅 6.6MB,零模型下载,开箱即用)。

## 快速开始

```bash
cargo build --release -p gh-rag-mcp
# 前置:一个 OpenAI 兼容嵌入 API 的 key(默认硅基流动免费档 BAAI/bge-m3)
export GH_RAG_API_KEY=sk-xxx

# 索引文件:~/.gh-rag/index.sqlite(当前由既有 Python 存量构建;Rust sync 建库见 M2)
./target/release/gh-rag-mcp.exe   # stdio MCP server,挂到任意 MCP 客户端
```

## 接入 MCP 客户端(OMP/Claude Code 等)

```json
{
  "mcpServers": {
    "gh-rag": {
      "type": "stdio",
      "command": "C:/Users/<you>/.gh-rag/bin/gh-rag-mcp.exe",
      "env": { "GH_RAG_API_KEY": "sk-xxx" },
      "timeout": 90000
    }
  }
}
```

agent 获得四个工具(签名冻结,见 DESIGN §3.8):`search_issues` / `get_issue_context` / `find_related` / `list_repos`。

## 嵌入后端(全部走 HTTP,exe 永远 6.6MB)

| 后端 | 配置 | 场景 |
|---|---|---|
| 硅基流动(默认) | `GH_RAG_API_KEY` | 日常,免费 bge-m3 |
| 任意 OpenAI 兼容端点 | 加 `GH_RAG_API_BASE` | vLLM / TEI / 网关 |
| ollama(本机) | `GH_RAG_API_BASE=http://127.0.0.1:11434/v1` | 断网/隐私;对齐验证见 `.github/workflows/ollama-align.yml`(手动触发) |

## 诊断

```bash
# API 报障一键定位(status/headers/body 全量)
GH_RAG_API_KEY=xxx cargo run -p gh-rag-core --example api_probe --release

# API 黄金对齐(需网络 + key;守护嵌入与索引同空间,阈值 0.999)
GH_RAG_API_KEY=xxx cargo test -p gh-rag-core --features golden -- --nocapture
```

## 开发

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings
cargo test --workspace
```

架构纪律与里程碑见 [AGENTS.md](AGENTS.md)。
