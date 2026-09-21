"""硅基流动双模型能力对比(嵌入 + 重排),dbx 真实数据。

用法:
    GH_RAG_API_KEY=sk-xxx .venv/Scripts/python scripts/compare_models.py

A. 嵌入对比:BAAI/bge-m3(免费,1024d)vs Qwen/Qwen3-VL-Embedding-8B(¥0.7/M)
   - dbx 前 1000 条 issue 各建内存索引
   - 12 个中英混合真实查询,各自 top-5
   - 输出并排结果与重合度
B. 重排对比:BAAI/bge-reranker-v2-m3(免费)vs Qwen/Qwen3-VL-Reranker-8B(付费)
   - 用 bge-m3 召回 top-10,两个 reranker 各排一次
   - 输出排序差异与分数
"""
from __future__ import annotations

import json
import os
import sqlite3
import sys
import time
from pathlib import Path

import httpx
import numpy as np

BASE = "https://api.siliconflow.cn/v1"
KEY = os.environ.get("GH_RAG_API_KEY", "")
N_ISSUES = 1000
BATCH = 64

QUERIES = [
    "连接 postgres 数据库失败报错",
    "导出查询结果到 Excel 文件",
    "table structure editor slow and unresponsive",
    "TDengine",
    "连接分组功能 减少分组",
    "AI 助手聊天记录丢失",
    "暗黑模式 主题切换",
    "mongodb 连接超时",
    "SQL 自动补全慢",
    "数据库结构对比缺少注释",
    "ssh 隧道连接",
    "redis 查看键值",
]


def http() -> httpx.Client:
    return httpx.Client(
        base_url=BASE,
        headers={"Authorization": f"Bearer {KEY}"},
        timeout=120.0,
    )


def embed(client: httpx.Client, model: str, texts: list[str]) -> np.ndarray:
    out = []
    for i in range(0, len(texts), BATCH):
        r = client.post(
            "/embeddings", json={"model": model, "input": texts[i : i + BATCH]}
        )
        r.raise_for_status()
        data = r.json()["data"]
        out.extend(d["embedding"] for d in data)
        print(f"  embed {model.split('/')[-1]}: {min(i + BATCH, len(texts))}/{len(texts)}", end="\r")
    print()
    mat = np.array(out, dtype=np.float32)
    mat /= np.linalg.norm(mat, axis=1, keepdims=True) + 1e-12
    return mat


def rerank(client: httpx.Client, model: str, query: str, docs: list[str]) -> list[tuple[int, float]]:
    r = client.post(
        "/rerank",
        json={"model": model, "query": query, "documents": docs, "top_n": len(docs),
              "return_documents": False},
    )
    r.raise_for_status()
    results = r.json()["results"]
    return [(x["index"], x["relevance_score"]) for x in results]


def load_dbx(limit: int) -> list[dict]:
    db_path = Path.home() / ".gh-rag" / "index.sqlite"
    db = sqlite3.connect(db_path)
    rows = db.execute(
        "SELECT repo, number, title, body FROM issues ORDER BY number DESC LIMIT ?",
        (limit,),
    ).fetchall()
    db.close()
    return [
        {"repo": r[0], "number": r[1], "title": r[2], "body": (r[3] or "")[:2000]}
        for r in rows
    ]


def build_text(title: str, body: str) -> str:
    return f"{title}\n{title}\n{body}"


def top_k(mat: np.ndarray, q: np.ndarray, k: int) -> list[tuple[int, float]]:
    sims = mat @ q
    order = np.argsort(-sims)[:k]
    return [(int(i), float(sims[i])) for i in order]


def main() -> int:
    if not KEY:
        print("GH_RAG_API_KEY not set")
        return 1

    issues = load_dbx(N_ISSUES)
    texts = [build_text(i["title"], i["body"]) for i in issues]
    print(f"loaded {len(issues)} dbx issues (~{sum(len(t) for t in texts) // 4 / 1e6:.1f}M tokens/模型)")

    client = http()

    print("\n== 嵌入模型 A:BAAI/bge-m3(免费)==")
    t0 = time.time()
    m_bge = embed(client, "BAAI/bge-m3", texts)
    t_bge = time.time() - t0

    print("\n== 嵌入模型 B:Qwen/Qwen3-VL-Embedding-8B(付费)==")
    try:
        m_qwen = embed(client, "Qwen/Qwen3-VL-Embedding-8B", texts)
    except Exception as e:
        print(f"Qwen 嵌入不可用({type(e).__name__};付费模型需账户余额),跳过嵌入对比")
        m_qwen = None

    print(f"dim: bge={m_bge.shape[1]}")
    q_bge = embed(client, "BAAI/bge-m3", QUERIES)

    if m_qwen is not None:
        print(f"dim: qwen={m_qwen.shape[1]}")
        q_qwen = embed(client, "Qwen/Qwen3-VL-Embedding-8B", QUERIES)
        overlaps = []
        print("\n================ 检索质量并排(top-5)================")
        for qi, query in enumerate(QUERIES):
            hits_bge = top_k(m_bge, q_bge[qi], 5)
            hits_qwen = top_k(m_qwen, q_qwen[qi], 5)
            ov = len({i for i, _ in hits_bge} & {i for i, _ in hits_qwen})
            overlaps.append(ov)
            print(f"\nQ: {query}   [top5 重合 {ov}/5]")
            for label, hits in (("bge ", hits_bge), ("qwen", hits_qwen)):
                tops = " | ".join(
                    f"#{issues[i]['number']}{issues[i]['title'][:22]}" for i, _ in hits[:3]
                )
                print(f"  {label}: {tops}")
        print(f"\n平均 top-5 重合度: {np.mean(overlaps):.1f}/5")
    else:
        print("\n(无 Qwen 对比)展示 bge-m3 单模型检索结果:")
        for qi, query in enumerate(QUERIES):
            hits = top_k(m_bge, q_bge[qi], 3)
            tops = " | ".join(f"#{issues[i]['number']}{issues[i]['title'][:26]}" for i, _ in hits)
            print(f"  Q: {query[:30]:<32} -> {tops}")

    # ---- rerank 对比 ----
    print("\n================ 重排对比(bge-m3 召回 top-10)================")
    for query in QUERIES[:5]:
        qi = QUERIES.index(query)
        cand = top_k(m_bge, q_bge[qi], 10)
        docs = [f"{issues[i]['title']}\n{issues[i]['body'][:300]}" for i, _ in cand]
        lines = []
        for name, model in (
            ("bge-reranker ", "BAAI/bge-reranker-v2-m3"),
            ("qwen-reranker", "Qwen/Qwen3-VL-Reranker-8B"),
        ):
            try:
                ranked = rerank(client, model, query, docs)
                top3 = " | ".join(
                    f"#{issues[cand[i][0]]['number']}({s:.2f})" for i, s in ranked[:3]
                )
            except Exception as e:
                top3 = f"FAILED: {type(e).__name__}"
            lines.append(f"  {name}: {top3}")
        print(f"\nQ: {query}")
        print(lines[0])
        print(lines[1])


if __name__ == "__main__":
    raise SystemExit(main())
