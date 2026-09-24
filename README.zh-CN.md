# gh-rag

[![CI](https://github.com/alex-1263/github-rag/actions/workflows/ci.yml/badge.svg)](https://github.com/alex-1263/github-rag/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/alex-1263/github-rag)](https://github.com/alex-1263/github-rag/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-stable-orange.svg)](https://www.rust-lang.org)
[![MCP](https://img.shields.io/badge/MCP-server-green.svg)](https://modelcontextprotocol.io)

**[English](README.md) | 中文**

**给 AI agent 的 GitHub issue/PR 语义记忆层。** 提出问题,它找回相关的 issue、讨论与修复 PR——跨仓库、跨语言、不经关键词。

## 它解决什么

仓库的 issue 区是一座金矿,但 agent 挖不动它:关键词搜索看不懂"连接失败"和 *"database unreachable"* 是一回事,更不知道三个月前有个 PR 已经修过它。结果就是——同样的 bug 被重复报告,agent 重复造轮子。

gh-rag 把整个仓库的 issue、PR 和讨论变成可语义检索的记忆,通过 [MCP](https://modelcontextprotocol.io) 挂进任何 agent:

```
> search_issues("agent 如何记住对话历史 memory")     ← 中文查询

  [langchain] #2792  agent memory
  [langchain] #197   Harrison/agent memory
  [langchain] #9681  initialize_agent not saving and returning messages
        ↑ 跨语言命中英文仓库,零关键词重叠

> get_issue_context("t8y2/dbx", 51)                   ← 上下文包

  relations: { "fixed_by": [{ "number": 55 }] }        ← 哪个 PR 修了它,直接给出
  comments:  "[- t8y2] 已兼容修复,下版本包含"          ← 维护者的结论
```

实测基线(真实 agent 查询 × LLM 裁判):**nDCG@5 = 0.949,MRR = 1.000,垃圾率 0%**。

## 能力

- **issue + PR 同场检索** + **关联图**(fixes/closes/fixed_by——"哪个 PR 修了它"一步到位)
- **讨论全收录**——评论进入检索并随上下文返回
- **混合检索**——语义(向量)+ 关键词(BM25,CJK 双字组分词)+ RRF 融合
- **单条级增量**——内容或评论变了才重算那一条
- **质量飞轮**——query_log → `gh-rag report` 报表 → `gh-rag eval` 评测(nDCG/MRR/Hit,LLM 裁判 + 锚定校准)
- **零锁定**——嵌入后端随意换(百炼/硅基流动/ollama/任意 OpenAI 兼容),6MB 单二进制
- **骨架分发**——预构建索引([gh-rag-indexes](https://github.com/alex-1263/gh-rag-indexes)):`fetch` 装载向量(指纹校验)+ 本地 `sync` 补全文,**零嵌入成本**
- **国内网络韧性**——代理(`[network]` proxy 或 `HTTPS_PROXY`)、传输/读体退避重试、断点续拉、原子落盘

## 快速开始

```bash
# 1. 从 Release 下载二进制(或 cargo build --release -p gh-rag-mcp -p gh-rag-cli)

# 2. 配置 ~/.gh-rag/config.toml
cat > ~/.gh-rag/config.toml <<'TOML'
repos = ["owner/repo"]
[embedding]
provider = "aliyun"        # 或 siliconflow / ollama / custom
api_key = "sk-..."
[network]                   # 可选:国内直连 GitHub 不稳时
proxy = "socks5://127.0.0.1:10808"
TOML

# 3. 建索引并接入 MCP 客户端(Claude Code / OMP 等)
gh-rag sync --all           # 全量建库(issue + PR + 评论)
# mcp.json 的 command 指向 gh-rag-mcp,agent 即获得四工具:
# search_issues / get_issue_context / find_related / list_repos
```

装载预构建索引(可选,免嵌入费):

```bash
gh-rag fetch --from <gh-rag-indexes 的骨架库 URL>
gh-rag sync --all           # 补全文,向量零重嵌
```

日常:`gh-rag sync --all`(增量秒级)/ `gh-rag status` / `gh-rag report --days 7`(检索质量报表)/ `gh-rag doctor`。

## 工作方式

```
GitHub API ──(代理+重试+断点)──→ raw 原始层(gzip,只抓一次)──→ 嵌入 + 全文 ──→ index.sqlite
                                                                       ↓
                                    MCP 客户端 ←── 混合检索 + 关联图 + query_log
```

6MB 的 server 不含任何推理引擎——嵌入走 HTTP,换后端 = 改一行配置(指纹机制强制校验向量空间,防无声劣化)。

## 配置速查

```toml
repos = ["owner/repo"]
[embedding]   # provider/api_key/base_url/model/dimensions/batch_size/batch_interval_ms
[network]     # proxy = "socks5://..."
[retrieval]   # vec_top/fts_top/rrf_k/top_k/snippet_chars(MCP 检索参数)
[eval]        # judge_model/base_url/days(评测裁判,默认 qwen-flash)
```

环境变量 `GH_RAG_API_KEY` / `GH_RAG_API_BASE` / `GH_RAG_API_MODEL` / `GH_RAG_TOKEN` / `HTTPS_PROXY` 优先级最高。

## 开发

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings
cargo test --workspace        # ~100 测试,零网络
```

开发纪律与冻结契约见 [AGENTS.md](AGENTS.md),贡献请读 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 许可

[MIT](LICENSE)。注意:issue/PR 正文与讨论的版权属于各原作者,索引仅供本地使用,请勿整库再分发文本内容(骨架分发只含向量与元数据)。
