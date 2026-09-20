"""gh-rag CLI: init / sync / search / issue / related / serve / status / rebuild / doctor."""
from __future__ import annotations

import re
from pathlib import Path

import typer

from . import config as C
from .store import IssueStore
from .retrieve import hybrid_search, find_related

app = typer.Typer(add_completion=False, help="gh-rag: semantic memory over GitHub issues")

_REF_RE = re.compile(r"^([\w.-]+)/([\w.-]+)#(\d+)$")


# -- shared wiring ---------------------------------------------------------


def _store() -> IssueStore:
    return IssueStore(C.DB_PATH)


def _embedder():
    from .embed import BgeM3Embedder

    cfg = C.load_config()["embedding"]
    return BgeM3Embedder(
        model=cfg["model"],
        hf_mirror=cfg["hf_mirror"],
        batch_size=cfg["batch_size"],
        max_seq_len=cfg.get("max_seq_len", 512),
    )


def _parse_ref(ref: str) -> tuple[str, str, int]:
    m = _REF_RE.match(ref.strip())
    if not m:
        typer.secho(f"bad ref {ref!r}: expected owner/repo#123", fg=typer.colors.RED)
        raise typer.Exit(2)
    return m.group(1), m.group(2), int(m.group(3))


# -- commands --------------------------------------------------------------


@app.command()
def init():
    """Create ~/.gh-rag/, config template, and an empty index."""
    C.DATA_DIR.mkdir(parents=True, exist_ok=True)
    if not C.CONFIG_PATH.exists():
        C.CONFIG_PATH.write_text(C.CONFIG_TEMPLATE, encoding="utf-8")
        typer.echo(f"config template written: {C.CONFIG_PATH}")
    else:
        typer.echo(f"config exists: {C.CONFIG_PATH}")
    IssueStore(C.DB_PATH).close()
    typer.echo(f"index ready: {C.DB_PATH}")
    typer.echo("edit repos in config.toml, then: gh-rag sync --all")


@app.command()
def sync(
    repos: list[str] = typer.Argument(None, help="owner/repo slugs; omit with --all"),
    all: bool = typer.Option(False, "--all", help="sync every repo in config.toml"),
    full: bool = typer.Option(False, "--full", help="ignore cursor, re-fetch everything"),
):
    """Fetch issues (incremental by default) and (re)embed changed ones."""
    cfg = C.load_config()
    targets = list(repos or [])
    if all:
        targets.extend(r for r in cfg["repos"] if r not in targets)
    if not targets:
        typer.secho("no repos given and config.toml repos is empty", fg=typer.colors.YELLOW)
        raise typer.Exit(2)

    from .github import GithubClient

    client = GithubClient(cfg["token"])
    store = _store()
    emb = _embedder()
    rcfg = cfg["retrieval"]
    store.ensure_embedding_fp(emb.fingerprint())
    batch = rcfg.get("batch_size", 64) or 64

    for slug in targets:
        owner, name = slug.split("/", 1)
        cursor = None if full else store.get_cursor(slug)
        mode = "incremental" if cursor else "full"
        typer.echo(f"[{slug}] sync ({mode})...")
        n_new = n_skip = 0
        pending: list[dict] = []
        max_updated = cursor or ""

        def flush(pending):
            nonlocal n_new
            if not pending:
                return
            texts = [
                emb.build_text(
                    p["title"], p["body"],
                    rcfg["title_repeats"], rcfg["body_max_chars"],
                )
                for p in pending
            ]
            hashes = [emb.text_hash(t) for t in texts]
            vectors = emb.embed_texts(texts)
            for p, t, h, v in zip(pending, texts, hashes, vectors):
                store.upsert_issue(p, v, h)
                n_new += 1
            pending.clear()

        for item in client.iter_issues(owner, name, stop_before=cursor):
            text = emb.build_text(
                item["title"], item["body"],
                rcfg["title_repeats"], rcfg["body_max_chars"],
            )
            h = emb.text_hash(text)
            if store.existing_hash(slug, item["number"]) == h:
                n_skip += 1
                max_updated = max(max_updated, item["updated_at"])
                continue
            pending.append(item)
            max_updated = max(max_updated, item["updated_at"])
            if len(pending) >= batch:
                flush(pending)
                typer.echo(
                    f"[{slug}] progress: embedded={n_new} unchanged={n_skip} "
                    f"(latest: #{item['number']} {item['title'][:48]})"
                )
        flush(pending)
        if max_updated:
            store.set_cursor(slug, max_updated)
        typer.echo(f"[{slug}] done: embedded={n_new} unchanged={n_skip}")

    store.close()


@app.command()
def search(
    query: str = typer.Argument(...),
    repo: list[str] = typer.Option(None, "--repo", "-r", help="filter: owner/repo"),
    state: str = typer.Option("all", "--state", "-s", help="open|closed|all"),
    label: list[str] = typer.Option(None, "--label", "-l"),
    top_k: int = typer.Option(None, "--top-k", "-k"),
):
    """Hybrid semantic search (same engine the MCP tools use)."""
    cfg = C.load_config()
    store = _store()
    emb = _embedder()
    store.ensure_embedding_fp(emb.fingerprint())
    hits = hybrid_search(
        store, emb, query,
        repos=list(repo) if repo else None, state=state,
        labels=list(label) if label else None,
        top_k=top_k, cfg=cfg["retrieval"],
    )
    if not hits:
        typer.echo("no results")
        return
    for h in hits:
        typer.secho(
            f"{h['score']:.4f} [{h['source']:>7}] {h['repo']}#{h['number']} ({h['state']}) {h['title']}",
            fg=typer.colors.CYAN if h["source"] == "vec+fts" else typer.colors.WHITE,
        )
        if h["snippet"]:
            typer.echo(f"    {h['snippet'][:180]}")
    store.close()


@app.command()
def issue(ref: str = typer.Argument(..., help="owner/repo#123")):
    """Print the full context pack for one issue."""
    from .mcp_server import _core

    owner, name, num = _parse_ref(ref)
    pack = _core().get_issue_context(f"{owner}/{name}", num)
    if not pack or "error" in pack:
        typer.secho("not found — synced?", fg=typer.colors.RED)
        raise typer.Exit(1)
    typer.secho(f"## {pack['repo']}#{pack['number']} {pack['title']}", bold=True)
    typer.echo(f"state={pack['state']} labels={pack['labels']} comments={pack['comments_count']}")
    typer.echo("--- body ---")
    typer.echo((pack["body"] or "")[:4000])
    typer.echo("--- related ---")
    for r in pack["related"]:
        typer.echo(f"  {r['score']:.4f} {r['repo']}#{r['number']} {r['title']}")


@app.command()
def related(ref: str = typer.Argument(...), top_k: int = typer.Option(10, "-k")):
    """Nearest-neighbour issues (vector-only)."""
    owner, name, num = _parse_ref(ref)
    store = _store()
    hits = find_related(store, f"{owner}/{name}", num, top_k=top_k)
    for h in hits:
        typer.echo(f"{h['score']:.4f}  {h['repo']}#{h['number']}  {h['title']}")
    store.close()


@app.command()
def status():
    """Indexed repos, counts, freshness."""
    store = _store()
    for r in store.repo_stats():
        typer.echo(
            f"{r['repo']:>40}  issues={r['issues']:<6} last_sync={r['last_sync_at'] or '-'}"
        )
    fp = store.get_manifest("embedding_fp")
    typer.echo(f"embedding_fp: {fp}")
    store.close()


@app.command()
def rebuild():
    """Drop the index (issues are re-fetched on next sync)."""
    if not typer.confirm(f"delete {C.DB_PATH}?"):
        raise typer.Abort()
    for suf in ("", "-wal", "-shm"):
        p = Path(str(C.DB_PATH) + suf)
        if p.exists():
            p.unlink()
    typer.echo("index deleted; run gh-rag init && gh-rag sync --all")


@app.command()
def doctor():
    """Environment self-check: token, FTS5, model pin, db health."""
    ok = True
    cfg = C.load_config()
    if cfg["token"]:
        typer.secho(f"token: ok (len={len(cfg['token'])})", fg=typer.colors.GREEN)
    else:
        ok = False
        typer.secho("token: MISSING (set GH_RAG_TOKEN / config / gh auth login)", fg=typer.colors.RED)
    try:
        import sqlite3

        c = sqlite3.connect(":memory:")
        c.execute("CREATE VIRTUAL TABLE t USING fts5(x)")
        typer.secho("sqlite FTS5: ok", fg=typer.colors.GREEN)
    except Exception as e:
        ok = False
        typer.secho(f"sqlite FTS5: BROKEN ({e})", fg=typer.colors.RED)
    if C.DB_PATH.exists():
        store = _store()
        fp = store.get_manifest("embedding_fp")
        typer.echo(f"db: {C.DB_PATH} ({C.DB_PATH.stat().st_size // 1024} KB) fp={fp}")
        n = store.db.execute("SELECT COUNT(*) FROM issues").fetchone()[0]
        typer.echo(f"indexed issues: {n}")
        store.close()
    else:
        typer.echo("db: not created yet (run gh-rag init)")
    typer.echo(
        f"embedding model: {cfg['embedding']['model']} "
        f"(hf_mirror={cfg['embedding']['hf_mirror']}, "
        f"max_seq_len={cfg['embedding'].get('max_seq_len', 512)})"
    )
    raise typer.Exit(0 if ok else 1)


@app.command()
def serve(
    transport: str = typer.Option("stdio", "--transport", help="stdio|http (Phase 1)"),
):
    """Run the MCP server."""
    from .mcp_server import main as mcp_main

    mcp_main(transport)


if __name__ == "__main__":
    app()
