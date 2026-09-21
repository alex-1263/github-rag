# 免费/低成本嵌入与重排模型资源指南

> 2026-09 实测整理。本项目默认栈:**BAAI/bge-m3(嵌入)+ BAAI/bge-reranker-v2-m3(重排)**,两者在硅基流动免费托管,本地 ONNX 双备份。
> 本文回答:"我可以换哪些免费模型/平台?"

## 一、云端免费平台对比

| 平台 | 免费嵌入 | 免费重排 | 免费额度 | 限流 | 协议 | 本项目可直接用 |
|---|---|---|---|---|---|---|
| **硅基流动**(国内首选) | ✅ bge-m3 / bge-large-zh / bce-embedding | ✅ bge-reranker-v2-m3 / bce-reranker | 标价免费;**需实名 + 账户有余额**(赠送金不覆盖重排,充值 ¥10 即解锁全部) | embedding RPM≈2000 / TPM≈50万;rerank RPM 2000 / TPM 50万 | OpenAI 兼容 | ✅ **已实测**:黄金对齐 0.999936 |
| **Cloudflare Workers AI** | ✅ @cf/baai/bge-m3 | ⚠️ 仅 AI Search 内置,无通用 rerank API | 10,000 Neurons/天(约可嵌数万条) | 按日额度制 | 自有 REST(非 OpenAI 兼容) | ⚠️ 需适配层 |
| **NVIDIA NIM**(build.nvidia.com) | ✅ bge-m3 / NV-EmbedQA | ✅ NeMo Retriever Reranking | 免费 credits 池,无信用卡,限开发测试用途 | credits 制 | 部分 OpenAI 兼容 | ⚠️ 需验证 |
| **Jina AI** | ✅ jina-embeddings-v3/v4 | ✅ jina-reranker-v2 | 注册免费额度(百万 token 级) | 额度制 | 自有(类 Cohere) | ⚠️ 需适配层 |
| HuggingFace Inference | 部分 | 部分 | 每月少量,不稳定 | 低 | 自有 | ❌ 不适合生产 |
| 智谱 / OpenAI / Cohere / Voyage | ❌ 无免费层 | ❌(Cohere $2/1K 次) | — | — | OpenAI 兼容(智谱/OpenAI) | 付费备选 |

**国内网络结论**:硅基流动是唯一"免费 + OpenAI 兼容 + 直连无代理"的全家桶;Cloudflare/NVIDIA/Jina 需代理或海外网络。

## 二、付费参考价(超出免费额度后)

| 模型 | 价格 | dbx 规模(6385 条 ≈ 5M token) |
|---|---|---|
| 硅基流动 Qwen3-VL-Embedding-8B | ¥0.7/M(文本)、¥1.8/M(图像) | ¥3.5 |
| 智谱 embedding-3 | ~¥0.5/M | ¥2.6 |
| 阿里百炼 qwen3-rerank / gte-rerank-v2 | ~¥0.8/M | 重排按查询计,月费用个位数 |
| OpenAI text-embedding-3-small | $0.02/M | ~$0.1 |
| Cohere rerank-3.5 | $2/1K 次搜索 | 重排月 $90 级(贵 100 倍) |

实测结论(2026-09,dbx 1000 条 + 12 查询):**付费 8B 与免费 bge 系无代差,重排分数区分度反而 bge 更好**——该场景免费栈无短板。

## 三、本地方案(免费且无限量)

| 方案 | 模型 | 说明 |
|---|---|---|
| **本项目内置**(fp32 ONNX) | bge-m3 | `~/.gh-rag/models/bge-m3/`,与 Python 逐位一致(0.999999) |
| **本项目内置**(fastembed int8) | bge-m3 int8 | `~/.gh-rag/models/bge-m3-int8/`,对齐 0.978,提速 2.6× |
| fastembed TextCrossEncoder | bge-reranker-v2-m3 | Rust 内置支持,M1.5 启用插槽时零新依赖 |
| Ollama | bge-m3 / bge-reranker | `ollama pull bge-m3`,OpenAI 兼容端点 localhost:11434/v1 |

## 四、本项目接入速查

ApiEmbedder 走 OpenAI 兼容协议,三件套切换:

```bash
# 硅基流动(默认)
GH_RAG_API_KEY=sk-xxx  gh-rag-mcp.exe

# 任意 OpenAI 兼容平台(如 Ollama 本地,免 key 可填任意)
GH_RAG_API_BASE=http://localhost:11434/v1  GH_RAG_API_KEY=x  gh-rag-mcp.exe

# 换模型
GH_RAG_API_MODEL=BAAI/bge-large-zh-v1.5  ...
```

**换模型/换平台前必做黄金对齐验证**(防向量空间混用):

```bash
GH_RAG_API_KEY=xxx GH_RAG_API_BASE=... GH_RAG_API_MODEL=... \
  cargo test -p gh-rag-core --features api,golden --test golden_api -- --ignored --nocapture
```

判读:≥0.999 可复用现有库;0.99x 必须全库 rebuild(fp 会拒绝混用并报错)。

## 五、实测记录

| 日期 | 内容 | 结果 |
|---|---|---|
| 2026-09-20 | 硅基流动 bge-m3 vs 本地 fp32 | min cosine **0.999936**(实质等价,免 rebuild) |
| 2026-09-21 | bge-reranker vs Qwen3-VL-Reranker-8B | 排序一致;bge 分数 0.7–0.98 区分度 vs Qwen 压缩 0–0.25;免费版胜 |
| 2026-09-21 | Qwen3-VL-Embedding-8B 检索 | 指令式前缀必需(裸调用跑偏);加前缀后与 bge 各有胜负无代差 |
| 2026-09-21 | 硅基流动余额门槛 | 赠送金不可用于重排;充值 ¥10 解锁全部免费模型 |
