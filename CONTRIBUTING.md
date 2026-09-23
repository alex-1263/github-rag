# 贡献指南

感谢对 gh-rag 的关注!本项目处于验证期,功能面刻意克制——**新增功能请先开 issue 讨论**再动手。

## 开发环境

- Rust stable(Windows / Linux / macOS 均可)
- 无需任何模型文件或数据库服务,`cargo test --workspace` 即可跑全部测试

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings
cargo test --workspace
```

CI 三门禁(fmt / clippy `-D warnings` / 测试)全绿才可合并,`main` 分支有保护规则。

## 提交规范

- **提交信息用中文**(类型前缀和标识符除外):`<类型>:<描述>`,类型 = feat/fix/test/refactor/docs/ci/perf
- 示例:`fix:FTS 查询含括号时的语法错误`
- 一个提交只做一件事;bug 修复请附复现测试

## 测试纪律(TDD)

1. **红 → 绿 → 重构**:先有失败测试,再有实现;bug 修复必须先写能复现原 bug 的测试
2. 假 embedder / 假 GitHub API 做确定性测试(内容 hash 派生向量),集成测试零网络
3. 在线测试(黄金对齐)需 `GH_RAG_API_KEY`,不在常规 CI 跑:

```bash
GH_RAG_API_KEY=xxx cargo test -p gh-rag-core --features golden -- --nocapture
```

## 硬约束(违反 = 改动无效,详见 AGENTS.md)

- **黄金对齐阈值(0.999)永远不许放宽**:它守的是向量空间一致性,失败时检索质量会无声劣化
- **MCP 工具签名冻结**:改动须先改 DESIGN.md §3.8 契约再改代码
- **依赖方向单向**:cli/mcp → core;core 不依赖任何 bin
- **数据分层**:GitHub API → raw 原始层 → 索引;索引重建只许从 raw 走
- **禁止** `index.sqlite` 写入任何虚拟表(vec0 等语言私有格式)——跨语言兼容是设计决定
- core 内禁止 `unwrap()`(测试除外)、禁止隐式全局状态

## 数据贡献

索引数据(骨架库)的贡献见 [gh-rag-indexes](https://github.com/alex-1263/gh-rag-indexes):PR 加仓库名,或 fork 自助构建。骨架库只含向量与事实元数据,**禁止携带正文/评论文本**(版权红线,CI 自动校验)。

## 行为准则

正常人类标准:对事不对人,讨论用证据,分歧时数据说话。issue/评论内容版权属于各作者,勿整库再分发。
