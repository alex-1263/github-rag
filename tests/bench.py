"""Micro-benchmark: where does sync time actually go? embed / api / upsert."""
from __future__ import annotations

import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))

from gh_rag import config as C
from gh_rag.embed import BgeM3Embedder
from gh_rag.store import IssueStore
from gh_rag.github import GithubClient


def main():
    import torch

    print(f"torch {torch.__version__}, threads={torch.get_num_threads()}")

    cfg = C.load_config()
    emb = BgeM3Embedder(
        model=cfg["embedding"]["model"], hf_mirror=False,
        batch_size=cfg["embedding"]["batch_size"],
        max_seq_len=cfg["embedding"].get("max_seq_len", 512),
    )

    texts = [("Benchmark issue title %d\n" % i) * 2 + ("body text. " * 120)
             for i in range(128)]  # ~1000 chars each, realistic

    t0 = time.time(); emb.embed_texts(texts[:8]); warm = time.time() - t0
    print(f"warmup(8): {warm:.1f}s (includes model load)")

    t0 = time.time(); emb.embed_texts(texts); dt = time.time() - t0
    print(f"EMBED 128 items: {dt:.1f}s -> {128/dt:.1f} items/s")

    store = IssueStore(Path(os.environ.get("GH_RAG_HOME", "")) / "bench.sqlite"
                       if os.environ.get("GH_RAG_HOME") else Path("bench.sqlite"))
    vec = emb.embed_texts(texts[:1])[0]
    fake = {
        "repo": "bench/x", "number": 1, "title": "t", "body": "b", "state": "open",
        "labels": [], "author": "a", "comments_count": 0,
        "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z",
    }
    t0 = time.time()
    for i in range(64):
        fake["number"] = i
        store.upsert_issue(fake, vec, f"h{i}")
    print(f"UPSERT 64 rows: {time.time()-t0:.2f}s")

    client = GithubClient(cfg["token"])
    t0 = time.time()
    pages = 0
    for i, item in enumerate(client.iter_issues("t8y2", "dbx")):
        if i >= 200:
            break
        if i % 100 == 99:
            pages += 1
    print(f"API 200 issues ({pages+1} pages): {time.time()-t0:.1f}s")


if __name__ == "__main__":
    main()
