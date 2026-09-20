"""Hybrid retrieval: brute-force vector scan ∥ FTS5 -> RRF fusion."""
from __future__ import annotations

import numpy as np


def _fts_sanitize(query: str) -> str:
    """Wrap as an FTS5 phrase to dodge syntax errors from raw user input."""
    cleaned = " ".join(query.replace('"', " ").split())
    return f'"{cleaned}"'


def _rrf_fuse(vec_rank: dict[int, int], fts_rank: dict[int, int], k: int) -> dict[int, float]:
    scores: dict[int, float] = {}
    for rid, rank in vec_rank.items():
        scores[rid] = scores.get(rid, 0.0) + 1.0 / (k + rank)
    for rid, rank in fts_rank.items():
        scores[rid] = scores.get(rid, 0.0) + 1.0 / (k + rank)
    return scores


def hybrid_search(
    store,
    embedder,
    query: str,
    repos: list[str] | None = None,
    state: str | None = None,
    labels: list[str] | None = None,
    top_k: int | None = None,
    cfg: dict | None = None,
    log: bool = True,
) -> list[dict]:
    cfg = cfg or {}
    top_k = top_k or cfg.get("top_k", 5)

    # ① vector leg
    qvec = embedder.embed_query(query)
    rows = store.candidates(repos, state, labels)
    vec_rank: dict[int, int] = {}
    if rows:
        ids = np.array([r[0] for r in rows], dtype=np.int64)
        mat = np.frombuffer(b"".join(r[1] for r in rows), dtype=np.float32)
        mat = mat.reshape(len(rows), -1)
        sims = mat @ qvec
        order = np.argsort(-sims)[: cfg.get("vec_top", 30)]
        vec_rank = {int(ids[i]): r + 1 for r, i in enumerate(order)}

    # ② BM25 leg
    fts_rows = store.fts_search(
        _fts_sanitize(query), repos, state, limit=cfg.get("fts_top", 30)
    )
    fts_rank = {rid: r + 1 for r, (rid, _s) in enumerate(fts_rows)}

    # ③ fuse
    scores = _rrf_fuse(vec_rank, fts_rank, cfg.get("rrf_k", 60))
    top = sorted(scores.items(), key=lambda x: -x[1])[:top_k]

    metas = store.meta([i for i, _ in top])
    snip = cfg.get("snippet_chars", 200)
    out = []
    for iid, score in top:
        m = metas.get(iid)
        if not m:
            continue
        out.append(
            {
                "repo": m["repo"],
                "number": m["number"],
                "title": m["title"],
                "state": m["state"],
                "snippet": (m["body"] or "").strip()[:snip],
                "score": round(score, 5),
                "source": ("vec+fts" if iid in vec_rank and iid in fts_rank
                           else "vec" if iid in vec_rank else "fts"),
            }
        )
    if log:
        store.append_query_log(
            "search_issues", query,
            {"repos": repos, "state": state, "labels": labels, "top_k": top_k},
            [{"repo": o["repo"], "number": o["number"]} for o in out],
        )
    return out


def find_related(
    store, repo: str, number: int, top_k: int = 10, state: str | None = None
) -> list[dict]:
    """Nearest neighbours of one issue (vector-only, excludes itself)."""
    issue = store.get_issue(repo, number)
    if not issue:
        return []
    qblob = store.get_embedding(issue["id"])
    if qblob is None:
        return []
    qvec = np.frombuffer(qblob, dtype=np.float32)
    rows = store.candidates(None, state)
    ids = np.array([r[0] for r in rows], dtype=np.int64)
    mat = np.frombuffer(b"".join(r[1] for r in rows), dtype=np.float32).reshape(len(rows), -1)
    sims = mat @ qvec
    sims[ids == issue["id"]] = -1.0
    order = np.argsort(-sims)[:top_k]
    top_ids = [int(ids[i]) for i in order]
    metas = store.meta(top_ids)
    return [
        {
            "repo": metas[tid]["repo"],
            "number": metas[tid]["number"],
            "title": metas[tid]["title"],
            "score": round(float(sims[i]), 4),
        }
        for tid, i in zip(top_ids, order)
    ]
