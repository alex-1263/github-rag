# gh-rag

GitHub issue 的语义记忆层:跨仓库混合检索,CLI + MCP 双形态,供 Claude Code / Copilot 等 agent 消费。设计文档见 [DESIGN.md](DESIGN.md)。

## 快速开始(验证期,Python)

```bash
python -m venv .venv
source .venv/Scripts/activate   # Git Bash(Windows)
pip install -e .

gh-rag init                     # 生成 ~/.gh-rag/config.toml + 空索引
# 编辑 ~/.gh-rag/config.toml:
#   repos = ["owner/repo", ...]        # 你要索引的仓库
#   [embedding] hf_mirror = true       # 中国网络必改
gh-rag sync --all               # 全量首拉 + 嵌入(bge-m3 首次下载 ~2.2GB)
gh-rag search "登录后跳转错误"    # 人直接查(和 agent 同一引擎)
gh-rag doctor                   # 环境自检
```

Token 依次取自:`GH_RAG_TOKEN` 环境变量 > config.toml > `gh auth token`。

## 接入 Claude Code

```bash
claude mcp add gh-rag -- "/path/to/github-rag/.venv/Scripts/gh-rag" serve
```

agent 获得四个工具:`search_issues` / `get_issue_context` / `find_related` / `list_repos`。

## 嵌入模型

默认 `BAAI/bge-m3`(多语言,1024 维)。模型名 + 库版本钉死在索引 manifest 里——
更换模型后需 `gh-rag rebuild && gh-rag sync --full`,防止向量空间混用。

## Phase 0 验证目标

1. 挑 ≥2 个真实仓库 sync
2. `gh-rag search` 抽查 20 条真实查询的 top-5 命中
3. 接入 Claude Code,观察 agent 是否主动调用(查 `query_log` 表)
4. 记录 agent 因召回历史 issue 改变行为的实例
