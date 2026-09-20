"""MCP server: 4 tools over the same core the CLI uses."""
from __future__ import annotations

from functools import lru_cache

from mcp.server.fastmcp import FastMCP

mcp = FastMCP("gh-rag")


# -- lazy core (server starts fast; model loads on first embed) ------------


class _Core:
    def __init__(self):
        from . import config as C
        from .store import IssueStore
        from .embed import BgeM3Embedder

        self.cfg = C.load_config()
        self.store = IssueStore(C.DB_PATH)
        emb_cfg = self.cfg["embedding"]
        self.emb = BgeM3Embedder(
            model=emb_cfg["model"], hf_mirror=emb_cfg["hf_mirror"],
            batch_size=emb_cfg["batch_size"],
            max_seq_len=emb_cfg.get("max_seq_len", 512),
        )
        # Pin the vector space on first use; mismatch -> explicit error.
        self.store.ensure_embedding_fp(self.emb.fingerprint())
        # 冷启动消除:server 启动即后台加载模型(4.3GB,~25s),
        # 首次查询不再撞 MCP 超时。
        import threading

        def _warm():
            try:
                self.emb.embed_query("warmup")
            except Exception:
                pass

        threading.Thread(target=_warm, daemon=True).start()
        # Pin the vector space on first use; mismatch -> explicit error.
        self.store.ensure_embedding_fp(self.emb.fingerprint())

    # tool bodies ----------------------------------------------------------

    def search_issues(self, query, repos=None, state="all", labels=None, top_k=None):
        from .retrieve import hybrid_search

        return hybrid_search(
            self.store, self.emb, query,
            repos=repos, state=state, labels=labels,
            top_k=top_k or None, cfg=self.cfg["retrieval"],
        )

    def get_issue_context(self, repo: str, number: int):
        from .retrieve import find_related

        m = self.store.get_issue(repo, number)
        if not m:
            return {"error": f"{repo}#{number} not indexed (sync first)"}
        self.store.mark_follow_up(f"{repo}#{number}")
        return {
            "repo": repo,
            "number": number,
            "title": m["title"],
            "state": m["state"],
            "labels": m["labels"],
            "comments_count": m["comments_count"],
            "updated_at": m["updated_at"],
            "body": (m["body"] or "")[:8000],
            "related": find_related(self.store, repo, number, top_k=5),
            "relations": self.store.relations_of(repo, number),
        }

    def find_related(self, repo: str, number: int, top_k: int = 10):
        from .retrieve import find_related as fr

        return fr(self.store, repo, number, top_k=top_k)

    def list_repos(self):
        return self.store.repo_stats()


@lru_cache(maxsize=1)
def _core() -> _Core:
    return _Core()


# -- tool registration ------------------------------------------------------


@mcp.tool()
def search_issues(
    query: str,
    repos: list[str] | None = None,
    state: str = "all",
    labels: list[str] | None = None,
    top_k: int = 5,
) -> list[dict]:
    """Semantically search GitHub issues across the user's indexed repositories.

    Use BEFORE starting work on a feature or bug: check whether someone already
    reported it, find prior discussions, or locate related historical issues.
    Accepts natural-language queries (e.g. "login redirect loop after OAuth").
    Filters: repos (owner/repo slugs), state (open|closed|all), labels.
    """
    return _core().search_issues(query, repos, state, labels, top_k)


@mcp.tool()
def get_issue_context(repo: str, number: int) -> dict:
    """Full context pack for one issue: body, labels, top-5 related issues,
    and explicit relations (duplicate/sub-issue/linked PR).

    Call after search_issues when you need to dig into a specific hit.
    """
    return _core().get_issue_context(repo, number)


@mcp.tool()
def find_related(repo: str, number: int, top_k: int = 10) -> list[dict]:
    """Find issues semantically similar to a given one.

    Use for duplicate detection, understanding a bug family, or broadening
    a narrow search result.
    """
    return _core().find_related(repo, number, top_k)


@mcp.tool()
def list_repos() -> list[dict]:
    """List indexed repositories with issue counts and last-sync time.

    Use to check coverage/freshness before searching.
    """
    return _core().list_repos()


def main(transport: str = "stdio"):
    mcp.run(transport="stdio" if transport == "stdio" else "sse")


if __name__ == "__main__":
    main()
