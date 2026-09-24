# gh-rag 路线图与交接计划

> 面向新开发会话的执行文档:当前状态 → 下一步(Tier 1 带规格)→ 积压与触发条件 → 运维要点。
> 纪律基线见 [AGENTS.md](../AGENTS.md)(冻结契约/TDD/数据分层),本文档只管"做什么与顺序"。
> 更新:2026-09-24(v0.2.0 后)

## 当前状态快照

| 维度 | 状态 |
|---|---|
| 代码 | main = v0.2.0,~100 测试,CI 三门禁绿,分支保护(禁 force push/删除) |
| 功能 | 混合检索(CJK bigram)/ relations 关联图 / 评论全链路 / eval(LLM 裁判)/ report / 骨架分发(fetch/export) |
| 数据 | 双仓 44,885 文档(t8y2/dbx 9,945 + langchain 34,940)+ 60k 评论 + 3,289 关联 |
| 质量 | eval 基线:nDCG@5=0.949 / MRR=1.000 / 垃圾率 0%(15 题,锚定集待建) |
| 分发 | Release v0.2.0 三平台;gh-rag-indexes 周更(data-年-周,保留 4 期,已切 v0.2.0) |
| 验收计时 | kill criteria 裁决日 **2026-10-23**(对外可发现起 30 天);≥2 仓库已满足 |

## Tier 1 —— 下一波(全部合规面内,合计约一天)

### A. 新 MCP 工具 `check_duplicate(title, body)` ⭐ 项目初心
- **场景**:agent 帮用户起草 issue 时查重(PROPOSAL 第一痛点;dbx 维护者日常)
- **规格**:
  - 参数:`title: str, body: str, repos?: str[], top_k?: int = 5`
  - 行为:嵌入 `build_text(title, body)` → hybrid 检索 → 标题相似度加权(重复报告标题近似度高)→ 返回 `[{repo, number, kind, title, state, score, title_sim, source}]`
  - 新工具 = 契约**增量**(四个既有工具不动),工具描述写触发场景:"Before filing a new issue, check whether it already exists"
  - TDD:假 embedder 下重复标题命中测试 / 不相关不命中 / repos 过滤
- 新会话开工顺序:先改 AGENTS 冻结契约节(加第五工具)→ 再写测试 → 再实现

### B. `list_repos` 增量字段:labels 侧面表
- 返回体加 `labels: [{name, count}]`(全库 GROUP BY,SQL 一条)
- agent 从"瞎猜过滤值"变"按面值过滤";兼容面:纯增量字段
- 测试:多仓库多标签计数断言

### C. CLI `gh-rag search`(人类直查,欠了一路的 T2)
- 参数与 search_issues 完全同形,走同一 `hybrid_search_with_query`(含 query_log,tool=`cli-search`)
- 输出:人读表格(number/kind/repo/title/score/source)
- 验收:同一查询 CLI 与 MCP 结果一致,query_log 两条记录

## Tier 2 —— 有信号再做

| 项 | 触发条件 | 要点 |
|---|---|---|
| D. PR diff 摘要进 get_issue_context(`files` 字段) | agent 消费 relations 后追问"怎么修的"的实例出现 | sync 拉 /files 端点存 raw;get_issue_context 对 kind=pr 增返 `[{filename, additions, deletions, patch_head}]`;diff 是 Apache 授权内容,无版权问题 |
| E. `get_issues([numbers])` 批量上下文 | agent 连拉 ≥3 条成为常态 | 新工具,批量省往返 |
| F. `sync --watch` | 忘刷索引导致数据过期的事故发生 | 简单轮询循环 + 退避,不引入常驻服务 |
| G. search 可选时间过滤(`updated_since?`) | 排障场景被时间维度卡住的真实案例 | **需先走冻结契约修订**(可选参数对老客户端兼容,但纪律要求先文档后代码) |

## 常备事项(不占开发档期)

1. **锚定集**:从 query_log 挑 10 条出候选+初标 → 用户复核(~10 分钟)→ `~/.gh-rag/anchors.json`;此后每次 eval 自动校准裁判,不一致 >20% 报告作废
2. **eval 节奏**:每周 `gh-rag eval --days 7` 跑一次基线对比;指标倒退 = 回归门禁触发
3. **query_log 积累**:OMP 日常使用即可;7 天后评测集从 15 题扩到 30+
4. **kill criteria(2026-10-23 裁决)**:0 外部用户 → 归档/重定位;届时看:star/fork/issue、indexes 仓库 PR、骨架下载量
5. **CI 成本**:indexes 周更全量重建 ≈ ¥3.3/周(langchain 为主);量再涨就做增量缓存(把 index.sqlite 缓存为 workflow artifact)
6. **发版 checklist**:`git tag -a vX.Y.Z -m "..."`(必须带注释!轻量 tag 不随 --follow-tags 推)→ 显式 `git push origin vX.Y.Z` → 等 release workflow 绿 → indexes 仓库 `GH_RAG_VERSION` 同步升版

## 运维要点(新会话必读)

- **环境**:Windows + Git Bash;cargo 全路径 `~/.cargo/bin/cargo`;本机 socks 代理 `socks5://127.0.0.1:10808`(v2rayN)
- **国内网络**:拉 GitHub 长流量必须走代理——config `[network] proxy` 或 env `HTTPS_PROXY`;客户端已有传输/读体退避 + 断点(raw 逐页落盘)+ 原子写 + 挂起硬超时,五层韧性齐备
- **密钥位置**:`~/.gh-rag/config.toml`(嵌入 key/代理/评测裁判配置;仓库内无任何密钥——保持)
- **并行开发模式**:dev 分支 + 每 worktree 一 agent + **文件边界切分防冲突**;合并后必跑**二进制级全链回归**(fetch→sync→MCP→report)——单元绿灯在集成边界撒过两次谎,勿省
- **生产部署**:`~/.gh-rag/bin/gh-rag-mcp.exe`(OMP mcp.json 指向;换文件用 rename 腾位法,Windows 不能删运行中 exe)
- **数据重建**:raw 层(gzip JSONL)在 → 索引随时零 API 重建;`rm index.sqlite && gh-rag sync --all`
