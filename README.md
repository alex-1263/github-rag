# gh-rag

[![CI](https://github.com/alex-1263/github-rag/actions/workflows/ci.yml/badge.svg)](https://github.com/alex-1263/github-rag/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-stable-orange.svg)](https://www.rust-lang.org)
[![MCP](https://img.shields.io/badge/MCP-server-green.svg)](https://modelcontextprotocol.io)

**给 AI agent 的 GitHub issue 语义记忆层。** 提出问题，它找回相关的 issue、讨论与修复 PR——跨仓库、跨语言、不经关键词。

## 它解决什么

仓库的 issue 区是一座金矿，但 agent 挖不动它：关键词搜索看不懂"连接失败"和 *"database unreachable"* 是一回事，更不知道三个月前有个 PR 已经修过它。结果就是——同样的 bug 被重复报告，agent 重复造轮子。

gh-rag 把整个仓库的 issue、PR 和讨论变成可语义检索的记忆，通过 [MCP](https://modelcontextprotocol.io) 挂进任何 agent。agent 检索"导出乱码"，它得到的不只是匹配的 issue 报告，还有**维护者的修复结论和修复它的 PR**：

```
> search_issues("修复 导出 乱码")

  #144  [issue] [Bug] 查询结果有中文的时候导出 CSV 中文乱码
  #4065 [pr]    Fix:修复 MySQL 整库导出重复分号与导入拆句错误
  #1028 [issue] [🐞 Bug] 连接数据库后无法展开存储过程
        讨论:  [- t8y2] 已兼容修复，下版本包含        ← agent 直接拿到答案
```

## 能力

- **issue + PR 同场检索** —— 问题报告与修复方案一起找到
- **讨论全收录** —— 评论(含维护者结论)进入检索并随上下文返回
- **混合检索** —— 语义(向量)+ 关键词(BM25)+ RRF 融合，中英互查不漏专有名词
- **单条级增量** —— 内容变了才重算那一条，其余零开销
- **零锁定** —— 嵌入后端随意换(阿里云百炼 / 硅基流动 / ollama / 任意 OpenAI 兼容端点)，6MB 单二进制不绑定任何模型
- **本地优先** —— 全部数据在一个 SQLite 文件里；raw 原始层保证随时重建索引、零重复抓取

## 快速开始

```bash
# 1. 构建(需 Rust;之后只是两个单文件可执行)
cargo build --release -p gh-rag-mcp -p gh-rag-cli

# 2. 建索引(任选一个嵌入后端,填上 key)
cat > ~/.gh-rag/config.toml <<'TOML'
repos = ["t8y2/dbx"]
[embedding]
provider = "aliyun"        # 或 siliconflow / ollama / custom
api_key = "sk-..."
TOML
gh-rag sync --all          # 全量建库(issue + PR + 评论)

# 3. 挂进 MCP 客户端(如 Claude Code / OMP)
#    mcp.json 配置指向 gh-rag-mcp.exe,agent 获得四个工具:
#    search_issues / get_issue_context / find_related / list_repos
```

日常维护只需要 `gh-rag sync --all`(增量秒级)。别人建好的索引可以直接装载(骨架库=向量+元数据,本地补全文):

```bash
gh-rag fetch --from <骨架库 URL 或本地路径>   # 指纹校验,不匹配拒绝(防向量空间混用)
gh-rag sync --all                             # 补全文,向量零重嵌
```

预构建索引与数据贡献见 [gh-rag-indexes](https://github.com/alex-1263/gh-rag-indexes)(骨架库不含全文,版权边界即流程设计)。

## 工作方式

```
GitHub API ──→ raw 原始层(gzip,只抓一次)──→ 嵌入 + 全文 ──→ index.sqlite
                                                          ↓
                                    MCP 客户端 ←── 混合检索(向量+BM25+RRF)
```

**6MB 的 server 不含任何推理引擎**——嵌入走 HTTP 调用任何兼容端点，换后端 = 改一行配置(换向量空间时索引机制会强制重建，防止结果无声劣化)。

## 配置

`~/.gh-rag/config.toml`：

```toml
repos = ["owner/repo"]
[embedding]
provider = "aliyun"       # 内置模板:siliconflow / aliyun / ollama / openai / jina / custom
api_key = "sk-..."
# 可选覆盖:base_url / model / dimensions / batch_size / batch_interval_ms
```

后端速查：`aliyun`(qwen3.7-flash，¥0.125/M token)/ `siliconflow`(免费 bge-m3)/ `ollama`(本机断网可用)/ `custom`(vLLM、TEI、网关)。环境变量 `GH_RAG_API_KEY` / `GH_RAG_API_BASE` / `GH_RAG_API_MODEL` 优先级最高，便于脚本与 CI。

## 开发

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings
cargo test --workspace        # 33 个测试,零网络
gh-rag doctor                 # 配置自检 + 端点探活
```

设计细节见 [DESIGN.md](DESIGN.md)，开发纪律见 [AGENTS.md](AGENTS.md)，参与贡献请读 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 许可

[MIT](LICENSE)。注意：issue/PR 正文与讨论的版权属于各原作者，索引仅供本地使用，请勿整库再分发文本内容。
