"""Phase 0 smoke test: full pipeline on synthetic issues with a small model.

Run: .venv/Scripts/python tests/smoke.py
Uses a throwaway GH_RAG_HOME and paraphrase-multilingual-MiniLM (117MB) so the
pipeline is exercised end-to-end without the 2.2GB bge-m3 download.
"""
from __future__ import annotations

import os
import sys
import tempfile
from pathlib import Path

# Redirect data dir BEFORE importing gh_rag.config (module constants bind at import)
_TMP = tempfile.mkdtemp(prefix="gh-rag-smoke-")
os.environ["GH_RAG_HOME"] = _TMP
if not os.environ.get("GITHUB_ACTIONS"):  # CI runner 直连 HF 更快,本地走镜像
    os.environ.setdefault("HF_ENDPOINT", "https://hf-mirror.com")

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))

from gh_rag import config as C  # noqa: E402
from gh_rag.embed import BgeM3Embedder  # noqa: E402
from gh_rag.store import IssueStore  # noqa: E402
from gh_rag.retrieve import hybrid_search, find_related  # noqa: E402

SMALL_MODEL = "paraphrase-multilingual-MiniLM-L12-v2"

ISSUES = [
    ("acme/web", 1, "Login redirect loop after OAuth", "Users report being redirected back to the login page infinitely after completing OAuth. Happens with Google SSO when session cookie expires.", "open", ["bug", "auth"]),
    ("acme/web", 2, "PDF export missing Chinese characters", "Exporting reports to PDF renders Chinese text as squares. English content exports fine. Font embedding seems broken.", "open", ["bug", "export"]),
    ("acme/api", 3, "Memory leak in connection pool", "Long-running workers grow RSS by ~50MB/hour. Heap dump shows unclosed connections piling up in the pool after 429 retries.", "closed", ["bug", "performance"]),
    ("acme/web", 4, "Dark mode toggle resets on refresh", "Theme preference is not persisted; every page load resets to light mode. LocalStorage key is never written.", "open", ["bug", "ux"]),
    ("acme/api", 5, "Add bulk import endpoint", "Feature request: import thousands of records via CSV upload. Current single-record API is too slow for migration scenarios.", "open", ["enhancement"]),
    ("acme/web", 6, "认证后重定向循环(与 #1 相似但中文报告)", "使用企业微信扫码登录后,浏览器在登录页无限跳转。Cookie 过期后必现。", "open", ["bug", "auth"]),
    ("acme/api", 7, "Rate limit headers missing on v2 endpoints", "X-RateLimit-Remaining is absent on /v2/* responses, clients cannot back off properly and get 429 storms.", "closed", ["bug", "api"]),
    ("acme/web", 8, "Export to Excel strips formulas", "xlsx export flattens formula cells to static values. Users need computed columns preserved.", "open", ["bug", "export"]),
    ("acme/api", 9, "Webhook retries cause duplicate side effects", "At-least-once delivery triggers double processing because handlers are not idempotent.", "open", ["bug", "webhooks"]),
    ("acme/web", 10, "支持表格内联编辑", "希望列表页的表格支持单元格直接编辑并批量保存,而不是逐条打开详情页。", "open", ["enhancement", "ux"]),
]

QUERY_TO_TOP3 = [
    ("login redirect infinite loop", "acme/web#1"),
    ("登录后无限跳转", "acme/web#6"),
    ("pdf 中文乱码", "acme/web#2"),
    ("memory grows over time", "acme/api#3"),
]


def main() -> int:
    # _Core() reads config; point it at the small model so fingerprints match
    C.DATA_DIR.mkdir(parents=True, exist_ok=True)
    C.CONFIG_PATH.write_text(
        f'repos = []\ntoken = ""\n[embedding]\nmodel = "{SMALL_MODEL}"\n'
        "hf_mirror = true\nbatch_size = 8\n",
        encoding="utf-8",
    )
    print(f"[smoke] GH_RAG_HOME={_TMP}")
    emb = BgeM3Embedder(model=SMALL_MODEL, hf_mirror=False, batch_size=8)
    store = IssueStore(C.DB_PATH)
    fp = emb.fingerprint()
    store.ensure_embedding_fp(fp)
    print(f"[smoke] embedding fp: {fp}")

    rcfg = dict(C.DEFAULTS["retrieval"])

    for repo, num, title, body, state, labels in ISSUES:
        text = emb.build_text(title, body, rcfg["title_repeats"], rcfg["body_max_chars"])
        vec = emb.embed_texts([text])[0]
        store.upsert_issue(
            {
                "repo": repo, "number": num, "title": title, "body": body,
                "state": state, "labels": labels, "author": "smoke",
                "comments_count": 0, "created_at": "2026-09-01T00:00:00Z",
                "updated_at": "2026-09-02T00:00:00Z",
            },
            vec, emb.text_hash(text),
        )
    n = store.db.execute("SELECT COUNT(*) FROM issues").fetchone()[0]
    print(f"[smoke] indexed issues: {n}")
    assert n == len(ISSUES)

    failures = []
    for query, expect in QUERY_TO_TOP3:
        hits = hybrid_search(store, emb, query, top_k=3, cfg=rcfg)
        got = [f"{h['repo']}#{h['number']}" for h in hits]
        mark = "OK " if expect in got else "MISS"
        if expect not in got:
            failures.append((query, expect, got))
        print(f"[smoke] {mark} q={query!r} expect~{expect} got={got}")

    rel = find_related(store, "acme/web", 1, top_k=3)
    print(f"[smoke] related(#1): " + ", ".join(f"{r['repo']}#{r['number']}({r['score']})" for r in rel))
    rel_slugs = {f"{r['repo']}#{r['number']}" for r in rel}
    if "acme/web#6" not in rel_slugs:
        failures.append(("related#1->#6 cross-lingual", "acme/web#6", sorted(rel_slugs)))

    logs = store.db.execute(
        "SELECT tool, query, follow_up FROM query_log ORDER BY id"
    ).fetchall()
    print(f"[smoke] query_log rows: {len(logs)} (last: {logs[-1][1][:30]!r})")
    assert len(logs) >= len(QUERY_TO_TOP3), "query_log not recording searches"

    # MCP core tools (no server process; same objects serve/ uses)
    from gh_rag.mcp_server import _core
    core = _core()
    hits = core.search_issues("export 乱码 方块", top_k=3)
    slugs = [f"{h['repo']}#{h['number']}" for h in hits]
    print(f"[smoke] mcp.search_issues: {slugs}")
    pack = core.get_issue_context("acme/web", 2)
    assert "PDF" in (pack.get("body") or ""), "context pack body missing"
    print(f"[smoke] mcp.get_issue_context#2: related={len(pack['related'])} relations={len(pack['relations'])}")
    repos = core.list_repos()
    print(f"[smoke] mcp.list_repos: {[(r['repo'], r['issues']) for r in repos]}")
    fu = store.db.execute(
        "SELECT follow_up FROM query_log WHERE follow_up IS NOT NULL"
    ).fetchall()
    print(f"[smoke] follow_up marked: {fu}")

    store.close()
    if failures:
        print(f"\n[smoke] FAILURES ({len(failures)}):")
        for f in failures:
            print("  -", f)
        return 1
    print("\n[smoke] ALL PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
