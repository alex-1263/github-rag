# AGENTS.md — gh-rag 开发纪律(对所有 AI 编码 agent 生效)

> 本文档是本项目 AI 辅助开发的硬约束。规则冲突时,以本文档为准;本文档与 DESIGN.md 冲突时,停下来问人。

## 项目一句话

GitHub issue/PR 的语义记忆层:跨仓库混合检索(向量 + BM25 + RRF),CLI 与 MCP 双形态,供 AI agent 消费。设计全貌见 `DESIGN.md`。

## 分支模型(2026-09 架构收敛后)

- 单一主线 `main`:**纯 Rust + 纯 API 嵌入**。Python 验证版与本地 ONNX 推理已整体退役(决策记录见 DESIGN 头部),仓库不再有平行分支职责。
- `index.sqlite` schema 不变(普通表 + BLOB 向量列 + FTS5,禁止语言私有/虚拟表格式),存量索引继续可用。

## 里程碑(顺序执行,不许跳)

1. ~~M1 serve-only 对齐~~ **已完成**(2026-09):Rust 读库 + 混合检索逐条对齐 Python
2. ~~M2 sync 建库~~ **已完成**(2026-09):Rust 全量/增量建库;raw 原始层(gzip)+ 评论全链路 +
   PR 入库(kind);qwen3.7-flash 为当前默认嵌入(百炼)
3. **M2.5 relations 落地**:PR 的 fixes/closes 提及 → relations 表;get_issue_context 返回关联图
4. **M3 发布工程**:GoReleaser 多平台产物;索引分发走 Release asset(摘要+溯源,全文再分发踩版权线)
5. M4+(远期,先不做):web 查看端(axum 薄壳,复用 core)

## TDD 硬纪律(违反 = 改动无效)

1. **红 → 绿 → 重构**,每个功能 commit 先有失败测试,再有实现。测试与实现不同 commit 也行,但 PR 里测试必须先于实现出现。
2. **bug 修复必须先写复现测试**(能失败地复现原 bug),再修。修完测试进回归集。
3. **黄金对齐测试永远不许跳过、不许放宽**:fixtures(`tests/fixtures/golden_embeddings.json`,≥10 条中英混合文本的 fp32 参考向量,Python 侧生成后**冻结为永久基准**)。任何嵌入实现(当前 `ApiEmbedder`;候选 ollama)对每条输出余弦 **> 0.999** 才算过。这是向量空间一致性的唯一防线——它失败时,检索质量会**无声劣化**。跑法:本地 `GH_RAG_API_KEY=xxx cargo test -p gh-rag-core --features golden`;ollama 端点对齐走 `.github/workflows/ollama-align.yml`(手动触发)。
4. **MCP 工具签名与 CLI 参数形状已冻结**(见 DESIGN §3.8),对它们的行为改动必须先改契约文档再改代码——重构期内默认不许改。
5. manifest 纪律:任何写入向量的一方必须 `ensure_embedding_fp`;指纹不匹配一律报错拒绝,不许静默重建。

## 测试分层(写测试前先选层)

| 层 | 位置 | 测什么 | 依赖 |
|---|---|---|---|
| 单元 | 同文件 `#[cfg(test)]` | 纯逻辑(RRF 融合、文本组装、引用解析、游标推进) | 无 IO |
| 集成 | `crates/gh-rag-core/tests/*.rs`(一行为一文件) | search(检索质量/过滤/query_log)、sync_build(建库→增量→评论驱动→raw 重建) | 临时目录 + 假 embedder/假 API |
| **黄金对齐** | `crates/gh-rag-core/tests/api_golden.rs` | ApiEmbedder(或任意端点)vs 冻结 fixtures 余弦 > 0.999 | 在线 API(fixtures 为 bge-m3 空间;qwen 空间的基准待生成) |
| 快照 | 集成测试内 `insta` | 检索输出格式(排序、字段、截断) | 假 embedder |

测试纪律:假 embedder 返回确定性向量(如内容 hash 派生),保证测试可重复;需要真实模型的只有黄金层。

## 架构规则

```
crates/
  gh-rag-core/    # 全部领域逻辑。bin 之外唯一允许被依赖的 crate
    config.rs     # provider 预设/维度/批量/节流配置解析
    github.rs     # GithubApi trait + cursor 分页 HTTP 实现(issue/PR + 仓库级评论)
    raw.rs        # raw 原始层(JSONL gzip):重建索引零 API 的数据底座
    sync.rs       # 编排:拉取→raw→hash 增量判定(含评论)→批量嵌入→upsert
    api_embedder.rs / embedder.rs / retrieve.rs / store.rs
  gh-rag-cli/     # bin:clap → core(sync/status/doctor)
  gh-rag-mcp/     # bin:rmcp 工具注册 → core
  gh-rag-web/     # (M4)axum → core
```

数据分层纪律:**GitHub API → raw 层(唯一拉取点)→ 索引**。任何索引重建只许从 raw 走,禁止绕过 raw 直连 API 建库。

- **依赖方向单向**:cli/mcp/web → core。core 不依赖任何 bin。
- **core 的 IO 全部 trait 化**:`Embedder`(embed_texts/embed_query/fingerprint)、`IssueStore`(upsert/candidates/fts/meta)、`GithubApi`(iter_issues)。core 内禁止直接 `reqwest`/模型加载——具体实现放在 core 的 `infra` 模块,通过构造函数注入。
- 检索参数(RRF k、召回深度、截断)从 config 读,不写死——两分支必须读同一份 `~/.gh-rag/config.toml`。
- 新增第三方依赖须在 PR 描述里给一句话理由;`core` 的直接依赖目标 ≤ 10 个。
- 错误处理:core 用 `thiserror` 类型化错误;bin 层负责转成人话退出码。禁止 `unwrap()` 出现在 core(测试代码除外)。

## 命令速查

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings
cargo test --workspace            # 单元 + 集成(黄金层需在线,不含在内)
cargo run --bin gh-rag-mcp        # MCP serve(stdio)

# CLI(bin gh-rag)
gh-rag sync <repo> | --all        # 全量/增量同步;清 sync_state 后跑 = 全量重建
gh-rag status / doctor

# API 黄金对齐(需在线 + key)
GH_RAG_API_KEY=xxx cargo test -p gh-rag-core --features golden -- --nocapture

# API 报障探针(status/headers/body 一屏)
GH_RAG_API_KEY=xxx cargo run -p gh-rag-core --example api_probe --release
```

CI(`main`):fmt + clippy + test 全绿才可合并;API 黄金对齐需 key,不在常规 CI 跑;ollama 对齐为独立手动 workflow。

## 禁忌清单

- ❌ 跳过或放宽黄金对齐阈值(0.999)
- ❌ 在 core 里加载本地模型 / 读环境变量之外的隐式全局状态(嵌入走 API,HTTP 客户端在 core 的 api_embedder,禁止再引入模型运行时)
- ❌ 改 MCP 工具签名 / CLI 参数形状(冻结至 M3 后统一评审)
- ❌ 向 index.sqlite 写 vec0 或任何语言私有虚拟表
- ❌ 向仓库引入 Python / 双语言维护(2026-09 已退役,索引重建与评测全走 Rust)
- ❌ 提交信息用英文(本项目全中文,类型前缀和标识符除外)
- ❌ 无测试的实现改动直接合并

## 提交规范

中文提交信息,格式:`<类型>:<描述>`,类型 = feat/fix/test/refactor/docs/ci/perf。
示例:`test:黄金对齐测试先行(fixtures 由 Python 生成)`、`feat:core 的 RRF 融合实现`。
