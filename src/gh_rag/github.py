"""GitHub GraphQL client: incremental issue sync with rate-limit awareness."""
from __future__ import annotations

import time
from typing import Iterator

import httpx

GRAPHQL_URL = "https://api.github.com/graphql"

ISSUES_QUERY = """
query($owner: String!, $name: String!, $cursor: String, $first: Int!) {
  repository(owner: $owner, name: $name) {
    issues(
      first: $first
      after: $cursor
      orderBy: {field: UPDATED_AT, direction: DESC}
    ) {
      totalCount
      nodes {
        number
        title
        body
        state
        createdAt
        updatedAt
        author { login }
        comments { totalCount }
        labels(first: 20) { nodes { name } }
      }
      pageInfo { endCursor hasNextPage }
    }
  }
}
"""

PAGE_SIZE = 100
PAGE_SLEEP_S = 0.2          # 二级限流规避:翻页间隔
QUOTA_FLOOR = 100           # remaining 低于此值则等到 reset


class RateLimitError(RuntimeError):
    pass


class GithubClient:
    def __init__(self, token: str):
        if not token:
            raise RuntimeError(
                "no GitHub token: set GH_RAG_TOKEN, config.toml token, or run `gh auth login`"
            )
        self.http = httpx.Client(
            base_url="https://api.github.com",
            headers={
                "Authorization": f"Bearer {token}",
                "Content-Type": "application/json",
            },
            timeout=30.0,
        )

    # -- low level ---------------------------------------------------------

    def _respect_quota(self, headers: httpx.Headers) -> None:
        remaining = headers.get("x-ratelimit-remaining")
        reset = headers.get("x-ratelimit-reset")
        if remaining is not None and int(remaining) <= QUOTA_FLOOR and reset:
            wait = max(int(reset) - int(time.time()) + 2, 5)
            print(f"[gh-rag] rate limit low ({remaining}); sleeping {wait}s")
            time.sleep(wait)

    def _gql(self, variables: dict) -> dict:
        resp = self.http.post("/graphql", json={"query": ISSUES_QUERY, "variables": variables})
        self._respect_quota(resp.headers)
        if resp.status_code == 403:
            raise RateLimitError(f"403 from GitHub API: {resp.text[:200]}")
        resp.raise_for_status()
        data = resp.json()
        if data.get("errors"):
            raise RuntimeError(f"GraphQL errors: {data['errors']}")
        return data["data"]

    # -- sync --------------------------------------------------------------

    def iter_issues(
        self,
        owner: str,
        name: str,
        stop_before: str | None = None,
        max_pages: int = 500,
    ) -> Iterator[dict]:
        """Yield issue dicts, newest-updated first.

        stop_before: ISO timestamp cursor. Pages are walked until we see an
        issue updated at/before it (incremental mode) or pages run out
        (full mode). Duplicate/missing edge cases from mid-walk updates are
        tolerated because upsert is idempotent.
        """
        cursor: str | None = None
        repo_slug = f"{owner}/{name}"
        for page in range(max_pages):
            data = self._gql(
                {"owner": owner, "name": name, "cursor": cursor, "first": PAGE_SIZE}
            )
            issues = data["repository"]["issues"]
            nodes = issues["nodes"] or []
            if not nodes:
                return
            for n in nodes:
                if stop_before and n["updatedAt"] <= stop_before:
                    return  # 增量结束:后面都是旧的
                yield {
                    "repo": repo_slug,
                    "number": n["number"],
                    "title": n["title"] or "",
                    "body": n["body"] or "",
                    "state": n["state"].lower(),
                    "labels": [l["name"] for l in (n["labels"]["nodes"] or [])],
                    "author": (n["author"] or {}).get("login", ""),
                    "comments_count": n["comments"]["totalCount"],
                    "created_at": n["createdAt"],
                    "updated_at": n["updatedAt"],
                }
            info = issues["pageInfo"]
            if not info["hasNextPage"]:
                return
            cursor = info["endCursor"]
            time.sleep(PAGE_SLEEP_S)
        print(f"[gh-rag] {repo_slug}: hit max_pages={max_pages}, stopping (re-run to continue)")
