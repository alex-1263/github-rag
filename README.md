# gh-rag

[![CI](https://github.com/alex-1263/github-rag/actions/workflows/ci.yml/badge.svg)](https://github.com/alex-1263/github-rag/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-stable-orange.svg)](https://www.rust-lang.org)
[![MCP](https://img.shields.io/badge/MCP-server-green.svg)](https://modelcontextprotocol.io)

GitHub issue/PR 的语义记忆层:跨仓库混合检索(向量 + BM25 + RRF),MCP 形态供 Claude Code / Copilot / OMP 等 agent 消费。设计文档见 [DESIGN.md](DESIGN.md)。

**形态:纯 Rust 单二进制 + 纯 API 嵌入**。默认接入阿里云百炼 `qwen3.7-text-embedding-flash`(128K 上下文,¥0.125/M token;新用户总额 1M token 免费额度,**非每月刷新**),亦可一键切换硅基流动 / ollama / 任意 OpenAI 兼容端点。

## 能力

- **issue + PR 全量入库**(kind 区分,state 可过滤;PR 与 issue 同场检索——"某类报错"能同时命中 issue 报告与修复 PR)
- **评论全链路**:仓库级评论端点采集(含维护者修复结论)→ 嵌入文本聚合(bot 过滤)→ 落库 → `get_issue_context` 直接返还给 agent
- **raw 原始层**:抓取数据 gzip 存档(`~/.gh-rag/raw/`),**重建索引零 API 拉取**(换模型/改参数纯本地)
- **单条级增量**:内容/评论变化只重嵌变化的那条(sha1 内容哈希判定)
- **图片降噪**:嵌入文本剥离图片 URL(哈希噪声),原文保留供多模态 agent 消费

## 快速开始

```bash
cargo build --release -p gh-rag-mcp -p gh-rag-cli

# 配置 ~/.gh-rag/config.toml(见下),或环境变量:
#   GH_RAG_API_KEY / GH_RAG_API_BASE / GH_RAG_API_MODEL

gh-rag doctor               # 配置自检 + 端点探活
gh-rag sync t8y2/dbx        # 全量首建 / 增量同步(游标 + hash 跳过)
gh-rag status               # 索引状态
```

## 配置(`~/.gh-rag/config.toml`)

```toml
repos = ["owner/repo"]       # sync --all 使用

[embedding]
provider = "aliyun"          # 内置模板:siliconflow / aliyun / ollama / openai / jina / custom
api_key = "sk-..."           # 或环境变量 GH_RAG_API_KEY(优先级更高)
# 以下均可选(覆盖模板默认):
# base_url = "https://..."   # 含专用推理端点(独占算力)
# model = "qwen3.7-text-embedding-flash"
# dimensions = 1024          # 仅部分模型支持(256~2560;实测 1024 即甜点)
# batch_size = 16            # 百炼上限 16,硅基流动 64
# batch_interval_ms = 500    # 批间节流(免费档建议 4000)
```

## 接入 MCP 客户端

```json
{
  "mcpServers": {
    "gh-rag": {
      "type": "stdio",
      "command": "/path/to/gh-rag-mcp.exe",
      "timeout": 90000
    }
  }
}
```

四个工具(签名冻结):`search_issues` / `get_issue_context`(含评论讨论) / `find_related` / `list_repos`。返回体带 `kind`(issue/pr)与 `state`。

## 嵌入后端

| 后端 | 配置 | 场景 |
|---|---|---|
| 阿里云百炼(默认) | `provider = "aliyun"` | qwen3.7-flash,128K 窗口 |
| 硅基流动 | `provider = "siliconflow"` | 免费 bge-m3(需实名+余额) |
| ollama(本机) | `provider = "ollama"` | 断网/隐私;与 bge-m3 黄金基准对齐 0.999990 |
| 自建(vLLM/TEI) | `provider = "custom"` + `base_url` | 内网/自托管 |

切换后端 = 换向量空间,**指纹机制强制要求重建索引**(`rm ~/.gh-rag/index.sqlite && gh-rag sync --all`,raw 层保证零 API 拉取)。

## 诊断与对齐

```bash
# API 报障一键定位(status/headers/body)
GH_RAG_API_KEY=xxx cargo run -p gh-rag-core --example api_probe --release

# API 黄金对齐(bge-m3 端点,frozen fixtures,阈值 0.999)
GH_RAG_API_KEY=xxx cargo test -p gh-rag-core --features golden -- --nocapture

# ollama 对齐:手动触发 .github/workflows/ollama-align.yml
```

## 开发

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings
cargo test --workspace
```

架构纪律与里程碑见 [AGENTS.md](AGENTS.md)。
