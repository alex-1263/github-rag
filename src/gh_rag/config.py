"""Configuration: paths, config.toml loading, token resolution chain, defaults."""
from __future__ import annotations

import os
import subprocess
import tomllib
from pathlib import Path

DATA_DIR = Path(os.environ.get("GH_RAG_HOME", str(Path.home() / ".gh-rag")))
DB_PATH = DATA_DIR / "index.sqlite"
CONFIG_PATH = DATA_DIR / "config.toml"


def _apply_hf_mirror_early() -> None:
    """Set HF_ENDPOINT at the earliest possible moment.

    config.py is the first gh_rag module imported by every entrypoint
    (cli / mcp_server / tests). HF libraries bind their endpoint constants
    at import time, so this must run before any of them are imported.
    """
    try:
        if CONFIG_PATH.exists():
            with open(CONFIG_PATH, "rb") as f:
                cfg = tomllib.load(f)
            if cfg.get("embedding", {}).get("hf_mirror"):
                os.environ["HF_ENDPOINT"] = "https://hf-mirror.com"
    except Exception:
        pass  # config 不可读时退回默认端点


_apply_hf_mirror_early()

DEFAULTS: dict = {
    "retrieval": {
        "vec_top": 30,            # 向量召回深度
        "fts_top": 30,            # BM25 召回深度
        "rrf_k": 60,              # RRF 常数
        "top_k": 5,
        "title_repeats": 2,       # 嵌入文本:标题重复次数(信息加权)
        "body_max_chars": 2000,   # 嵌入文本:正文截断
        "snippet_chars": 200,
        "sync_ttl_minutes": 10,   # lazy catch-up TTL(Phase 1 使用)
    },
    "embedding": {
        "model": "BAAI/bge-m3",
        "hf_mirror": False,       # 网络受限时置 true,走 hf-mirror.com
        "batch_size": 32,
    },
}

CONFIG_TEMPLATE = """\
# gh-rag 配置
# 同步哪些仓库(任意 org 组合,手动列出)
repos = [
  # "owner/repo",
]

# GitHub token:留空则依次尝试 环境变量 GH_RAG_TOKEN -> `gh auth token`
token = ""

[embedding]
model = "BAAI/bge-m3"
hf_mirror = false     # 中国网络建议 true
batch_size = 32

[retrieval]
vec_top = 30
fts_top = 30
rrf_k = 60
top_k = 5
title_repeats = 2
body_max_chars = 2000
snippet_chars = 200
sync_ttl_minutes = 10
"""


def resolve_token(cfg_token: str = "") -> str:
    """Token 解析链:GH_RAG_TOKEN env > config.toml > `gh auth token`."""
    if os.environ.get("GH_RAG_TOKEN"):
        return os.environ["GH_RAG_TOKEN"]
    if cfg_token:
        return cfg_token
    try:
        out = subprocess.run(
            ["gh", "auth", "token"], capture_output=True, text=True, timeout=10
        )
        if out.returncode == 0 and out.stdout.strip():
            return out.stdout.strip()
    except Exception:
        pass
    return ""


def load_config() -> dict:
    cfg: dict = {"repos": [], "token": ""}
    if CONFIG_PATH.exists():
        with open(CONFIG_PATH, "rb") as f:
            loaded = tomllib.load(f)
        cfg.update(loaded)
    merged = {
        "repos": cfg.get("repos", []),
        "token": resolve_token(cfg.get("token", "")),
    }
    for section in ("retrieval", "embedding"):
        d = dict(DEFAULTS[section])
        d.update(cfg.get(section, {}))
        merged[section] = d
    return merged
