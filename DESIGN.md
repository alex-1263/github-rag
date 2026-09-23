# github-rag 项目方案(v2.0,与 main 分支实现对齐)

> 本版整合 v1.2→v1.5 全部增补注记为连贯正文:技术描述以当前代码为准(2026-09-23),旧方案(本地推理/Python 验证版/GraphQL/ETag)已按实际落地情况改写或降级标注。
> 姊妹文档:`PROPOSAL.md`(评估输入版,含竞品调研细节)。

## 1. 一页纸摘要

**两个项目,一个引擎:**

- **项目 A「gh-rag」**:给多仓库维护者/开发者的 issue/PR 语义记忆层。跨仓库混合检索 + agent 上下文包,MCP 形态供 Claude Code / Copilot 等消费。开源(MIT)、不商业化、本地优先。
- **项目 B「bugcards」**:社区共建的生态级故障经验卡片库(markdown 仓库 + 程序化锚定字段 + 评审信任标记)。**冻结,A 验收通过后启动。**

**立项逻辑**:AI 时代 issue 爆炸是真实且加速的痛点;官方(Copilot 语义搜索 GA)覆盖单仓库 + Copilot 订阅者;**锚点是官方结构性不做的层:跨 org 聚合、私有数据自托管、任意 agent 的开放 MCP、长尾维护者。**

**核心纪律**(来自评估的最大教训):
1. 验证对象是「agent 消费 context pack 是否可测量提升开发质量」,不是「自己爱不爱用」
2. kill criteria 先于开发设定
3. 功能面克制:所有火过的 RAG 项目都死于膨胀

## 2. 背景与定位

### 2.1 问题

AI 编码普及导致开源 issue 爆炸:数量(门槛消失)、重复(相似 prompt 产出趋同)、质量(幻觉性 bug 报告)、对话污染。issue 库本质是三份未被利用的资产:**用户声音、机构记忆、故障史**——当前仅关键词可查。

### 2.2 竞争格局结论(2026-09 调研)

| 对手 | 状态 | 与我们的错位 |
|---|---|---|
| GitHub 官方 | 语义 issue 搜索已 GA(付费 Copilot 计划);duplicate detection preview | 单仓库、绑 Copilot 生态、不做跨 org/自托管/开放 MCP |
| unsight.dev | Nuxt 负责人个人项目,embedding+聚类 | 自用溢出,无 MCP/agent 接口 |
| 小项目群 | 查重 Action / FAISS MCP / 分析脚本 | 无一个做完整 |
| VoC SaaS(Enterpret 类) | 商业成立 | 进不了私有数据场景 |

### 2.3 差异化定位

```
跨 org 语义记忆 + 私有数据不出域 + 任意 agent 的 MCP 消费 + relations 关系图(M2.5)
```

## 3. 项目 A:现行实现设计

### 3.1 范围与非目标

**做(已做):**

| 能力 | 说明 |
|---|---|
| 多仓库同步 | REST issues API + 仓库级评论端点,双 since 游标增量;cursor 分页(Link header) |
| issue + PR 同库 | kind 区分(state 可过滤),与 issue 同检索流——「报错」与「修复它的 PR」同场命中 |
| 评论全链路 | 采集 → 嵌入聚合(bot 过滤)→ issue_comments 落库 → MCP 透出 |
| 语义索引 | **纯 API 嵌入**(provider 预设可切),向量存 BLOB 列 + FTS5(CJK bigram),单文件 index.sqlite |
| 混合检索 | 向量 + BM25 → RRF 融合 →(插槽)rerank;检索参数从 config 读 |
| MCP 服务 | 4 工具:search_issues / get_issue_context(含评论) / find_related / list_repos |
| raw 原始层 | gzip JSONL,API 只打一次,索引重建零拉取 |
| 骨架分发 | 向量+元数据(无全文)Release 资产 + `fetch` 安全导入 + 本地 sync 补全文(零重嵌) |
| 质量地基 | query_log(含 filters/follow_up 定向)+ `gh-rag report` 报表 |

**不做(裁决记录):** 聚类/晨报(评估否定)、自动查重/写操作、GitHub App/webhook(Phase 2)、Web UI(M4)、PR merged/closed 区分(需逐 PR 调 pulls API,成本不成比例)、base+delta 分发(骨架分发已覆盖)。

### 3.2 系统架构(现行)

```
GitHub API ──cursor 分页──→ raw 原始层(gzip JSONL,逐页落盘断点)
                                │  重建索引零 API
                                ▼
              build_text(标题×2 + 正文截断 + 评论聚合,图片URL剥离)
                                │  sha1 内容哈希(含评论)→ 变了才重嵌
                                ▼
             嵌入(HTTP:aliyun/siliconflow/ollama/custom,批量+429退避)
                                ▼
                    index.sqlite(issues/vec/FTS5/comments/manifest)
                                ▼
        MCP serve:向量召回 ∥ BM25召回(CJK bigram)→ RRF → query_log
```

### 3.3 数据 Schema(现行 DDL 摘要)

```sql
CREATE TABLE issues (
  id INTEGER PRIMARY KEY, repo TEXT, number INTEGER,
  kind TEXT DEFAULT 'issue',           -- issue | pr
  title TEXT, body TEXT, state TEXT, labels TEXT,  -- labels 为 JSON
  author TEXT, comments_count INTEGER,
  created_at TEXT, updated_at TEXT, embedded_hash TEXT,
  UNIQUE(repo, number));
CREATE TABLE issues_vec (issue_id INTEGER PRIMARY KEY, embedding BLOB NOT NULL);
CREATE VIRTUAL TABLE issues_fts USING fts5(title, body, content='');
  -- unicode61 + 应用层 CJK 双字组变换(索引与查询同变换);manifest fts_cjk 标记幂等迁移
CREATE TABLE issue_comments (issue_id, idx, author, body, PRIMARY KEY(issue_id, idx));
CREATE TABLE relations (repo, number, kind, target_repo, target_number, PRIMARY KEY(...));
  -- M2.5 落地(fixes/closes 提及图)
CREATE TABLE sync_state (repo TEXT PRIMARY KEY, cursor_updated_at TEXT, last_sync_at TEXT);
CREATE TABLE manifest (key TEXT PRIMARY KEY, value TEXT);
  -- embedding_fp(含组装参数)/ schema_version / fts_cjk
CREATE TABLE query_log (id, ts, tool, query, filters, results, follow_up);
```

写入规则:upsert 单事务(id 稳定,contentless FTS 删旧插新);嵌入跳过时评论仍落库(两动作解耦);MCP 检索只读。

### 3.4 同步引擎与限流(现行 vs 原承诺)

| 措施 | 状态 |
|---|---|
| 认证 PAT(GH_RAG_TOKEN > config > gh auth) | ✅ |
| 翻页间 200ms 进程级节流 | ✅ |
| 尊重 Retry-After(403/429 重试≤3) | ✅ |
| 配额感知(x-ratelimit-remaining <200 等待至 reset) | ✅ |
| 断点:逐页流式落 raw(中途崩溃已拉页不丢) | ✅ |
| ~~ETag 条件请求(304 不扣配额)~~ | ❌ **降级**:issues 列表端点响应体随游标变化,ETag 命中率存疑,暂不实现 |
| ~~跨 run 分页断点(page_cursor)~~ | ❌ **降级**:崩溃后重跑从游标重拉(raw 去重保证数据不重,API 配额重耗一次);20 万条级仓库再评估 |

### 3.5 检索管线与质量保证

**管线**:`title×2 + body 3000 + 评论配额 3000/单条500` 嵌入 → 向量 ∥ BM25(CJK bigram)各 top-30 → RRF → top-5。**Rerank 插槽保留默认关**(触发条件:top-5 垃圾率 >30%,以 report 数据裁决;免费 bge-reranker 区分度已实测优于付费 8B)。

**质量系统**:
1. ✅ 点击日志(query_log + follow_up 定向标记 + report 报表)
2. ⏳ 评测集(duplicate 对 + RAGAS)——**未建**,验收前落地
3. ⏳ 回归门禁(评测集就位后接 CI)

**性能红线**:单实例 ≤10 万条(6.6k 条实测库内检索 ~30ms);超限先降维(512 实测重叠 87%),再考虑 ANN。

### 3.6 MCP 工具签名(冻结,返回体向后兼容扩展)

```
search_issues(query, repos?, state?, labels?, top_k=5)
  → [{repo, number, kind, title, state, snippet, score, source}]
get_issue_context(repo, number)
  → {issue 全文, labels, comments(讨论,预算4000字符), related, relations}
find_related(repo, number, top_k=10) → [{repo, number, title, score}]
list_repos() → [{repo, issues, last_sync}]
```

### 3.7 技术栈(已收敛为单一形态)

纯 Rust(core/cli/mcp 三 crate,直接依赖 8 个):rusqlite(bundled+FTS5)、rmcp、ureq、serde/serde_json、toml、thiserror、sha1、flate2。嵌入走 HTTP 无本地推理;发布走 tag 触发的多平台 Release workflow(非 GoReleaser)。**Python 于 2026-09 退役**(验证使命完成,黄金 fixtures 冻结为其遗产)。

### 3.8 CLI(现行)

```
gh-rag sync <repo> | --all     # 全量/增量
gh-rag status / doctor / report --days N
gh-rag export --skeleton -o f  # 骨架导出(分发形态,唯一)
gh-rag fetch --from <url|path> # 骨架装载(指纹校验+安全导入)
```

`search`(人类直查)暂未提供——人类侧以 report/status 观察,直查命令待后续评审(MCP 工具与 CLI 共引擎,补齐成本低)。

### 3.9 数据分发(gh-rag-indexes)

骨架库 = **向量 + 元数据 + 嵌入哈希,不含任何正文/评论文本**(版权模型:issue 文字归各作者,ToS D.5 仅授权 use/display/perform/fork,整库复制无授权;向量/事实元数据为衍生数据,CC-BY-4.0)。

- 命名 `gh-rag-index-{repo}-{model}-{dim}-{date}.sqlite.gz`,文件名/manifest 指纹/fetch 装载校验三级对账
- 消费:fetch(只读白名单导入,零执行外来 SQL)→ 本地 sync 补全文(hash 对齐零重嵌,实测嵌入 0 次)
- 贡献:①repos.toml 加名(维护者 key 构建)②fork 自助(自配 EMBED_API_KEY,同一 workflow 即产出规格,PR 只交 manifest 条目)
- 安全红线:pull_request_target 永不用;PR 校验 job 零 secrets;CI 自动卡"含全文"骨架

## 4. 验收与退出标准(先于开发设定,不变)

**验收(MVP 完成的定义):**
1. 对 ≥2 个真实仓库连续自用 7 天
2. query_log 证明 agent 真实调用(非手动)
3. 20 条真实查询 top-5 命中抽查通过
4. 记录 ≥1 次「agent 因召回历史 issue 改变行为」

**kill criteria:**
- 上线 30 天 0 外部用户 → 归档或重新定位
- 3 名陌生多仓库维护者试用,7 天内全部弃用 → 假设证伪
- M2.5/Phase 2 启动条件 = 验收全过 + 外部使用者出现

## 5. 项目 B:二期设计(冻结)

**形态**:markdown 卡片仓库,一 bug 一文件。frontmatter:`id/symptoms/root_cause/fix_pr/affected_versions/stack[]/source_issue/reviewed/reviewer` + 正文叙事。

**生产管道**(复用 A 资产):`label:bug 且有关联修复 PR` 入口 → LLM 结构化抽取(「放弃」合法)→ 双模型交叉验证 + 程序化锚定 → `reviewed:false` 入库 → 社区审核翻 true。

**版权纪律**(2026-09 核实 ToS 后定稿):卡片只取**思想/事实三要素**(症状/根因/修法,改写而非摘录),溯源链接即署名;截图只链接不搬运;申诉下架通道;仅公开仓库。

**诚实预期**:全自动质量天花板 80-90 分;审核供给是 B 最大结构性风险——这也是 B 冻结的原因。

## 6. 路线图(现状)

| 阶段 | 状态 |
|---|---|
| Phase 0-1.5(Python 验证 + MVP + 验收基建) | ✅ 完成(2026-09,Python 已退役) |
| M1 serve 对齐 / M2 建库 | ✅ 完成 |
| M2.5 relations(fixes/closes 图) | ⏳ 下一个 |
| M3 发布工程 | ✅ 部分(v0.1.0 多平台 Release + gh-rag-indexes 数据分发已运营;GoReleaser 不再需要) |
| Phase 2b(webhook 实时/rerank 启用) | 视验收数据 |
| Phase 3(B 启动) | 视外部验证 |

## 7. 风险登记册(浓缩四份评估)

| 风险 | 等级 | 对策 |
|---|---|---|
| 官方功能挤压 | 高 | 锚定结构性层(2.3);不做重叠功能 |
| 自用验证盲区 | 高 | kill criteria 前置;report 数据裁决 |
| 单人维护负担 | 高 | 单引擎复用;依赖 8 个;功能面克制 |
| embedding 一致性 | 中 | 指纹含组装参数,不符拒绝;黄金对齐(默认栈) |
| 法律(他人内容) | 低 | A 骨架分发零全文;B 卡片版权纪律(§5) |

## 8. 附录:采纳/拒绝清单

**采纳**:RAGAS 评估(待建)、reranker 默认插槽、存储窄接口、双级检索路由显式化。
**拒绝**:LLM 知识图谱抽取、深度文档解析、chunking 策略、WebUI/工作流、LangChain 系框架。

---

### 文档简史

- v1.0(2026-09-20):立项定稿,两步法(Python→Rust)
- v1.1-v1.2:两步法路线 + CLI/MCP 契约冻结
- v1.3(09-21):架构收敛——本地推理与 Python 退役,纯 Rust + 纯 API
- v1.4-1.4.2(09-23):PR/评论/raw 层/qwen 切换;索引分发定稿(骨架+补全文);数据贡献机制
- v1.5(09-23):外审修复(中文FTS/指纹参数/限流);默认 provider 定稿 siliconflow
- **v2.0(09-23):本版——注记折叠为正文,ETag/断点承诺降级标注,技术事实全面对齐现行实现**
