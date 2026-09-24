# gh-rag

[![CI](https://github.com/alex-1263/github-rag/actions/workflows/ci.yml/badge.svg)](https://github.com/alex-1263/github-rag/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/alex-1263/github-rag)](https://github.com/alex-1263/github-rag/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-stable-orange.svg)](https://www.rust-lang.org)
[![MCP](https://img.shields.io/badge/MCP-server-green.svg)](https://modelcontextprotocol.io)

**English | [中文](README.zh-CN.md)**

**Semantic memory over GitHub issues/PRs for AI agents.** Ask a question; get back the relevant issues, discussions, and the PRs that fixed them — across repositories, across languages, without keywords.

## The Problem It Solves

Your issue tracker is a gold mine that agents can't dig: keyword search doesn't know that "连接失败" and *"database unreachable"* mean the same thing, let alone that a PR fixed it three months ago. The result — the same bugs get re-reported, agents reinvent wheels.

gh-rag turns a repository's issues, PRs and discussions into semantically searchable memory, exposed to any agent via [MCP](https://modelcontextprotocol.io):

```
> search_issues("agent 如何记住对话历史 memory")      ← Chinese query

  [langchain] #2792  agent memory
  [langchain] #197   Harrison/agent memory
  [langchain] #9681  initialize_agent not saving and returning messages
        ↑ cross-language hits on an English repo, zero keyword overlap

> get_issue_context("t8y2/dbx", 51)                    ← context pack

  relations: { "fixed_by": [{ "number": 55 }] }         ← which PR fixed it, directly
  comments:  "[- t8y2] Fixed and compatible, next release"  ← maintainer's verdict
```

Measured baseline (real agent queries × LLM judge): **nDCG@5 = 0.949, MRR = 1.000, junk rate 0%**.

## Features

- **Issues + PRs in one search index**, plus a **relation graph** (fixes/closes/fixed_by — "which PR fixed this" in one step)
- **Full discussion ingestion** — comments are embedded, searchable, and returned with context
- **Hybrid retrieval** — semantic (vector) + keyword (BM25 with CJK bigram tokenization) + RRF fusion
- **Per-item incremental sync** — re-embeds only items whose content or comments changed
- **Quality flywheel** — query_log → `gh-rag report` → `gh-rag eval` (nDCG/MRR/Hit with an LLM judge + anchor calibration)
- **Zero lock-in** — swap embedding backends freely (Aliyun / SiliconFlow / ollama / any OpenAI-compatible endpoint); a single 6MB binary
- **Skeleton distribution** — prebuilt indexes ([gh-rag-indexes](https://github.com/alex-1263/gh-rag-indexes)): `fetch` loads vectors (fingerprint-verified) + local `sync` backfills full text — **zero embedding cost**
- **Hostile-network resilience** — proxy support (`[network]` proxy or `HTTPS_PROXY`), transport & body-read retries with backoff, resumable pulls, atomic writes

## Quick Start

```bash
# 1. Download a binary from Releases (or: cargo build --release -p gh-rag-mcp -p gh-rag-cli)

# 2. Configure ~/.gh-rag/config.toml
cat > ~/.gh-rag/config.toml <<'TOML'
repos = ["owner/repo"]
[embedding]
provider = "siliconflow"    # or aliyun / ollama / custom
api_key = "sk-..."
[network]                   # optional: if GitHub is unreachable directly
proxy = "socks5://127.0.0.1:1080"
TOML

# 3. Build the index and plug into an MCP client (Claude Code / OMP / ...)
gh-rag sync --all           # full build (issues + PRs + comments)
# point mcp.json's command at gh-rag-mcp; the agent gets four tools:
# search_issues / get_issue_context / find_related / list_repos
```

Loading a prebuilt index (optional, saves embedding cost):

```bash
gh-rag fetch --from <skeleton URL from gh-rag-indexes>
gh-rag sync --all           # backfill full text; zero re-embedding
```

Daily use: `gh-rag sync --all` (incremental, seconds) / `gh-rag status` / `gh-rag report --days 7` (retrieval quality report) / `gh-rag doctor`.

## How It Works

```
GitHub API ──(proxy+retries+resume)──→ raw archive (gzip, fetched once)──→ embed + text ──→ index.sqlite
                                                                                      ↓
                                   MCP clients ←── hybrid search + relation graph + query_log
```

The 6MB server contains no inference engine — embedding goes over HTTP; switching backends is a one-line config change (a fingerprint mechanism hard-verifies the vector space to prevent silent quality decay).

## Configuration Reference

```toml
repos = ["owner/repo"]
[embedding]   # provider/api_key/base_url/model/dimensions/batch_size/batch_interval_ms
[network]     # proxy = "socks5://..."
[retrieval]   # vec_top/fts_top/rrf_k/top_k/snippet_chars (MCP search parameters)
[eval]        # judge_model/base_url/days (eval judge, defaults to qwen-flash)
```

Environment variables `GH_RAG_API_KEY` / `GH_RAG_API_BASE` / `GH_RAG_API_MODEL` / `GH_RAG_TOKEN` / `HTTPS_PROXY` take highest precedence.

## Development

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings
cargo test --workspace        # ~100 tests, zero network
```

Design: [DESIGN.md](DESIGN.md) (Chinese). Discipline: [AGENTS.md](AGENTS.md). Contributing: [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[MIT](LICENSE). Note: issue/PR text and comments are copyrighted by their respective authors; indexes are for local use — do not redistribute full text (skeleton distribution contains vectors and metadata only).
