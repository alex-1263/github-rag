"""Generate golden embedding fixtures for Rust-Python alignment tests.

Run (main branch python env):
    .venv/Scripts/python scripts/gen_golden.py

Output: tests/fixtures/golden_embeddings.json
Contains >=12 mixed-language texts and their bge-m3 vectors (float32, 1024-dim,
L2-normalized) produced by the pinned Python environment. The Rust
implementation must reproduce these with cosine similarity > 0.999.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "src"))

from gh_rag import config as C  # noqa: E402
from gh_rag.embed import BgeM3Embedder  # noqa: E402

TEXTS = [
    "Login redirect loop after OAuth session expires",          # en bug title
    "连接 postgres 数据库失败报错",                                # zh query
    "Export query results to Excel file with formulas preserved",
    "希望连接导航栏的分组支持多级",                                # zh feature
    "TDengine super table display",                              # proper nouns
    "数据库表结构对比速度极慢且缺少注释",
    "Memory leak in connection pool after 429 retries",
    "redis 一键新建查询把当前 key 的语句填充到新建查询里面",
    "table structure editor slow and unresponsive on large schemas",
    "单元格详情弹窗里直接支持大文本编辑",
    "kafka 消息查询割裂感很重,不好用",                             # mixed punct
    "warmup",                                                     # trivial
]


def main() -> int:
    cfg = C.load_config()["embedding"]
    emb = BgeM3Embedder(
        model=cfg["model"], hf_mirror=cfg["hf_mirror"],
        batch_size=cfg["batch_size"], max_seq_len=cfg.get("max_seq_len", 512),
    )
    vecs = emb.embed_texts(TEXTS)
    out = {
        "model": cfg["model"],
        "max_seq_len": cfg.get("max_seq_len", 512),
        "sentence_transformers": _st_version(),
        "dimension": 1024,
        "note": "cosine(rust_output, reference) must be > 0.999 for every entry",
        "cases": [
            {"text": t, "vector": [round(x, 6) for x in _f32(v)]}
            for t, v in zip(TEXTS, vecs)
        ],
    }
    dest = ROOT / "tests" / "fixtures" / "golden_embeddings.json"
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_text(json.dumps(out, ensure_ascii=False), encoding="utf-8")
    print(f"wrote {len(TEXTS)} golden vectors -> {dest} ({dest.stat().st_size // 1024} KB)")
    print(f"fingerprint: {emb.fingerprint()}")
    return 0


def _f32(blob: bytes):
    import struct

    return struct.unpack(f"<{len(blob) // 4}f", blob)


def _st_version() -> str:
    try:
        import sentence_transformers as st

        return st.__version__
    except Exception:
        return "unknown"


if __name__ == "__main__":
    raise SystemExit(main())
