# AGENTS.md — gh-rag 开发纪律(对所有 AI 编码 agent 生效)

> 本文档是本项目 AI 辅助开发的硬约束。规则冲突时,以本文档为准;本文档与 DESIGN.md 冲突时,停下来问人。

## 项目一句话

GitHub issue 的语义记忆层:跨仓库混合检索(向量 + BM25 + RRF),CLI 与 MCP 双形态,供 AI agent 消费。设计全貌见 `DESIGN.md`。

## 分支模型(当前处于平行重构期)

- `main`:**Python 验证版,日常工具,只修 bug 不加新壳**。它支撑着真实使用与 `query_log` 验收数据,不许停摆。
- `rust-rewrite`:Rust 重构。**按里程碑推进,每个里程碑有对齐验收,不赌全量。**
- 两分支共用同一个 `index.sqlite`(schema 跨语言是设计决定:普通表 + BLOB 向量列 + FTS5,禁止引入语言私有格式)。

## 里程碑(顺序执行,不许跳)

1. **M1 serve-only 对齐**:Rust 读 Python 建的库,`embed_query` 输出与 Python 黄金向量余弦 > 0.999 → 替换 MCP serve
2. **M2 sync 建库**:Rust 端建库 + 增量,基准测试对比 Python 吞吐
3. **M3 全量替换 + 发布工程**:单二进制,GoReleaser,Python 退到 `scripts/`(评测/CI 辅助)
4. M4+(远期,先不做):web 查看端(axum 薄壳,复用 core)

## TDD 硬纪律(违反 = 改动无效)

1. **红 → 绿 → 重构**,每个功能 commit 先有失败测试,再有实现。测试与实现不同 commit 也行,但 PR 里测试必须先于实现出现。
2. **bug 修复必须先写复现测试**(能失败地复现原 bug),再修。修完测试进回归集。
3. **黄金对齐测试永远不许跳过、不许放宽**:`tests/golden_embeddings.json` 含 ≥10 条中英混合文本的 Python 参考向量(f32 数组,由 `scripts/gen_golden.py` 生成)。Rust `embed_query` 对每条输出余弦相似度 **> 0.999** 才算过。这是 tokenizer/预处理对齐的唯一防线——它失败时,检索质量会**无声劣化**,没有其他报警。
4. **MCP 工具签名与 CLI 参数形状已冻结**(见 DESIGN §3.8),对它们的行为改动必须先改契约文档再改代码——重构期内默认不许改。
5. manifest 纪律:任何写入向量的一方必须 `ensure_embedding_fp`;指纹不匹配一律报错拒绝,不许静默重建。

## 测试分层(写测试前先选层)

| 层 | 位置 | 测什么 | 依赖 |
|---|---|---|---|
| 单元 | 同文件 `#[cfg(test)]` | 纯逻辑(RRF 融合、文本组装、引用解析、游标推进) | 无 IO |
| 集成 | `crates/gh-rag-core/tests/*.rs`(一行为一文件) | 公共 API 行为:建库→检索→过滤→增量 | 临时目录 + 假 embedder |
| 契约 | `crates/gh-rag-mcp/tests/` | 四个 MCP 工具的输入输出形状 | 假 core |
| **黄金对齐** | `crates/gh-rag-core/tests/golden.rs` | Rust vs Python 向量逐条余弦 > 0.999 | **真实 ONNX 模型**(CI 单独 job,本地 `cargo test --features golden`) |
| 快照 | 集成测试内 `insta` | 检索输出格式(排序、字段、截断) | 假 embedder |

测试纪律:假 embedder 返回确定性向量(如内容 hash 派生),保证测试可重复;需要真实模型的只有黄金层。

## 架构规则

```
crates/
  gh-rag-core/    # 全部领域逻辑。bin 之外唯一允许被依赖的 crate
  gh-rag-cli/     # bin:clap 解析 → core。不许有业务逻辑
  gh-rag-mcp/     # bin:rmcp 工具注册 → core。不许有业务逻辑
  gh-rag-web/     # (M4)axum → core
```

- **依赖方向单向**:cli/mcp/web → core。core 不依赖任何 bin。
- **core 的 IO 全部 trait 化**:`Embedder`(embed_texts/embed_query/fingerprint)、`IssueStore`(upsert/candidates/fts/meta)、`GithubApi`(iter_issues)。core 内禁止直接 `reqwest`/模型加载——具体实现放在 core 的 `infra` 模块,通过构造函数注入。
- 检索参数(RRF k、召回深度、截断)从 config 读,不写死——两分支必须读同一份 `~/.gh-rag/config.toml`。
- 新增第三方依赖须在 PR 描述里给一句话理由;`core` 的直接依赖目标 ≤ 10 个。
- 错误处理:core 用 `thiserror` 类型化错误;bin 层负责转成人话退出码。禁止 `unwrap()` 出现在 core(测试代码除外)。

## 命令速查

```bash
# Rust(rust-rewrite 分支)
cargo fmt && cargo clippy --all-targets -- -D warnings
cargo test                        # 除黄金层外全部
cargo test --features golden      # 黄金对齐(需 ONNX 模型,首次自动下载)
cargo run -- sync t8y2/dbx        # CLI(M2 后)
cargo run --bin gh-rag-mcp        # MCP serve(M1 后)

# Python(main 分支,日常工具)
.venv/Scripts/gh-rag sync t8y2/dbx
.venv/Scripts/gh-rag search "查询"
.venv/Scripts/gh-rag status / doctor
.venv/Scripts/python tests/smoke.py
```

CI(main 分支):fmt + clippy + test 全绿才可合并;黄金对齐单独 job(有模型缓存)。rust-rewrite 分支 CI 同标准。

## 禁忌清单

- ❌ 跳过或放宽黄金对齐阈值(0.999)
- ❌ 在 core 里直接做网络请求 / 加载模型 / 读环境变量之外的隐式全局状态
- ❌ 改 MCP 工具签名 / CLI 参数形状(冻结至 M3 后统一评审)
- ❌ 向 index.sqlite 写 vec0 或任何语言私有虚拟表
- ❌ 在 main 分支加新功能壳(它只接 bug 修复)
- ❌ 提交信息用英文(本项目全中文,类型前缀和标识符除外)
- ❌ 无测试的实现改动直接合并

## 提交规范

中文提交信息,格式:`<类型>:<描述>`,类型 = feat/fix/test/refactor/docs/ci/perf。
示例:`test:黄金对齐测试先行(fixtures 由 Python 生成)`、`feat:core 的 RRF 融合实现`。
