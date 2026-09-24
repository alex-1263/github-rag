# AGENTS.md — gh-rag 开发纪律(对所有 AI 编码 agent 生效)

> 本文档是本项目 AI 辅助开发的硬约束。规则冲突时,以本文档为准;与现实冲突时,停下来问人。
> **[English](AGENTS.md) | 中文**

## 项目一句话

GitHub issue/PR 的语义记忆层:跨仓库混合检索(向量 + BM25 + RRF),CLI 与 MCP 双形态,供 AI agent 消费。

## 分支模型(2026-09 收敛后)

- 单一主线 `main`:**纯 Rust + 纯 API 嵌入**。Python 验证版与本地 ONNX 推理已退役;`index.sqlite` schema 保持跨语言(普通表 + BLOB 向量列 + FTS5,禁止语言私有虚拟表)。
- `dev` 为开发分支;功能在 dev 上进行(worktree 并行佳),门禁过后合入 main。

## 里程碑(顺序执行,不许跳)

1. ~~M1 serve-only 对齐~~ **已完成**——混合检索与 Python 基线逐条对齐
2. ~~M2 sync 建库~~ **已完成**——全量/增量同步、评论、PR(kind)、raw 原始层
3. ~~M2.5 relations~~ **已完成**——fixes/closes/refs 关联图经 get_issue_context 透出
4. ~~M3 发布工程~~ **已完成**——tag 触发多平台 Release;索引分发走 gh-rag-indexes(骨架 + 本地补全文)
5. M4+(远期):web 查看端(axum 薄壳复用 core)

## 硬规则(违反 = 改动无效)

1. **TDD,红 → 绿 → 重构**:每个功能 commit 先有失败测试;bug 修复先写复现测试。
2. **黄金对齐永远不许跳过、不许放宽**:冻结 fixtures(tests/fixtures/golden_embeddings.json)对任何嵌入实现余弦 > 0.999。这是防向量空间无声劣化的唯一防线。跑法:`GH_RAG_API_KEY=xxx cargo test -p gh-rag-core --features golden`。
3. **MCP 工具签名与 CLI 参数形状冻结**(见下方契约节)。行为改动必须先改契约再改代码。
4. **指纹纪律**:任何写入向量的一方必须过 `ensure_embedding_fp`;不匹配 = 硬报错,禁止静默重建。指纹含文本组装参数。
5. **数据分层**:GitHub API → raw 层(唯一拉取点)→ 索引。索引重建只许从 raw 走。
6. **依赖方向单向**:cli/mcp → core;core 不依赖任何 bin。core 的 IO 全 trait 化;禁止 reqwest、禁止模型运行时、core 内禁止 unwrap(测试除外)。

## 冻结契约:MCP 工具

```
search_issues(query, repos?, state?, labels?, top_k=5)
  → [{repo, number, kind, title, state, snippet, score, source}]
get_issue_context(repo, number)
  → {issue 正文, labels, comments, related, relations:{fixes, closes, fixed_by, refs}}
find_related(repo, number, top_k=10) → [{repo, number, title, score}]
list_repos() → [{repo, issues, last_sync}]
```

返回体允许增量扩展(kind/comments/relations 先例);参数形状不许动。

## 验收与 kill criteria(先于开发设定)

**验收(MVP 定义)**:≥2 真实仓库连续自用 7 天;query_log 证明 agent 真实调用;20 条真实查询 top-5 抽查通过;≥1 次「agent 因召回历史改变行为」的记录。

**kill criteria**:对外可发现后 30 天 0 外部用户 → 归档或重定位;3 名陌生维护者试用 7 天内全部弃用 → 假设证伪。

## 测试分层

| 层 | 位置 | 测什么 | 依赖 |
|---|---|---|---|
| 单元 | 同文件 `#[cfg(test)]` | 纯逻辑(RRF/bigram/提及解析/节流) | 无 IO |
| 集成 | `crates/gh-rag-core/tests/*.rs`(一文件一事):search/sync_build/skeleton_roundtrip/eval | 公共 API 端到端 | 临时目录 + 假 embedder/假 API |
| 黄金 | `tests/api_golden.rs` | 在线嵌入 vs 冻结 fixtures | 在线 + key |
| 二进制级 | 部署前手动全链回归 | 出厂二进制上 fetch→sync→MCP→report | 真实环境 |

测试纪律:确定性假件(内容哈希向量);CI 零网络;**出厂前二进制级回归**(单元绿灯会在集成边界撒谎——已被实锤两次)。

## 命令速查

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings
cargo test --workspace            # 单元 + 集成(黄金除外)
cargo run --bin gh-rag-mcp        # MCP serve(stdio)

gh-rag sync <repo> | --all        # 全量/增量
gh-rag status / doctor / report / eval
gh-rag export --skeleton -o f     # 骨架导出(分发形态)
gh-rag fetch --from <url|path>    # 骨架装载(指纹校验)

# 发版:必须带注释 tag(轻量 tag 不随 --follow-tags 推送)
git tag -a v0.x.y -m "..." && git push origin main v0.x.y
```

CI(main/dev):fmt + clippy + 测试全绿才可合并。黄金需 key 手动跑;ollama 对齐为手动 workflow。

## 禁忌清单

- ❌ 跳过或放宽黄金阈值(0.999)
- ❌ core 加载本地模型 / 环境变量之外的隐式全局状态
- ❌ 改 MCP 签名 / CLI 参数形状(冻结;返回体仅许增量字段)
- ❌ index.sqlite 写 vec0 或语言私有虚拟表
- ❌ 重新引入 Python / 双语言维护
- ❌ 任何 workflow 使用 pull_request_target(泄密经典)
- ❌ 提交信息用英文(本项目中文,`<类型>:<描述>`,类型 = feat/fix/test/refactor/docs/ci/perf)
- ❌ 无测试的实现改动直接合并
