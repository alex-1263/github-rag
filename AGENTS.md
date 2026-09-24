# AGENTS.md — gh-rag Development Discipline (applies to all AI coding agents)

> This document is the hard constraint for AI-assisted development of this project. On conflict, this document wins; if it conflicts with reality, stop and ask a human.
> **[English](AGENTS.md) | [中文](AGENTS.zh-CN.md)**

## The Project in One Line

A semantic memory layer over GitHub issues/PRs: cross-repository hybrid retrieval (vectors + BM25 + RRF), CLI + MCP surfaces, consumed by AI agents.

## Branch Model (post 2026-09 convergence)

- Single mainline `main`: **pure Rust + pure API embedding**. The Python prototype and local ONNX inference are retired; `index.sqlite` schema stays cross-language (plain tables + BLOB vectors + FTS5 — no language-private virtual tables).
- `dev` is the development branch; feature work lands on `dev` (worktrees encouraged), merges to `main` after gates pass.

## Milestones (in order, no skipping)

1. ~~M1 serve-only alignment~~ **done** — hybrid retrieval byte-aligned with the Python baseline
2. ~~M2 sync / index building~~ **done** — full/incremental sync, comments, PRs (kind), raw layer
3. ~~M2.5 relations~~ **done** — fixes/closes/refs graph surfaced via `get_issue_context`
4. ~~M3 release engineering~~ **done** — tag-triggered multi-platform Releases; index distribution via `gh-rag-indexes` (skeleton + local backfill)
5. M4+ (later): web viewer (axam thin shell over core)

## Hard Rules (violations = change is invalid)

1. **TDD, red → green → refactor.** Every feature commit has a failing test first. Bug fixes start with a reproducing test.
2. **Golden alignment is never skipped or relaxed**: frozen fixtures (`tests/fixtures/golden_embeddings.json`) must score cosine > 0.999 against any embedding implementation. This is the only guard against silent vector-space drift. Run: `GH_RAG_API_KEY=xxx cargo test -p gh-rag-core --features golden`.
3. **MCP tool signatures and CLI parameter shapes are frozen** (see Contract section below). Behavior changes require updating this contract first, then the code.
4. **Fingerprint discipline**: any writer of vectors must pass `ensure_embedding_fp`; mismatch = hard error, never silent rebuild. Fingerprints include text-assembly parameters.
5. **Data layering**: GitHub API → raw layer (the only fetch point) → index. Index rebuilds must go through raw, never direct-to-API.
6. **Dependency direction is one-way**: cli/mcp → core; core depends on no bin. Core I/O is trait-based; no `reqwest`, no model runtimes, no `unwrap()` in core (tests excepted).

## Frozen Contract: MCP Tools

```
search_issues(query, repos?, state?, labels?, top_k=5)
  → [{repo, number, kind, title, state, snippet, score, source}]
get_issue_context(repo, number)
  → {issue body, labels, comments, related, relations:{fixes, closes, fixed_by, refs}}
find_related(repo, number, top_k=10) → [{repo, number, title, score}]
list_repos() → [{repo, issues, last_sync}]
```

Return-body additive extensions (kind/comments/relations) are allowed; parameter shapes are not.

## Acceptance & Kill Criteria (set before development)

**Acceptance (MVP definition):** ≥2 real repos in daily use for 7 days; query_log proves real agent calls; top-5 spot checks pass on 20 real queries; ≥1 recorded case of an agent changing behavior due to recalled history.

**Kill criteria:** 30 days after public discoverability with 0 external users → archive or reposition; 3陌生 maintainers try it and all abandon within 7 days → hypothesis falsified.

## Testing Layers

| Layer | Location | Covers | Dependencies |
|---|---|---|---|
| Unit | `#[cfg(test)]` in-file | pure logic (RRF, bigram, mentions, throttling) | no I/O |
| Integration | `crates/gh-rag-core/tests/*.rs` (one concern per file): search, sync_build, skeleton_roundtrip, eval | public API behavior end-to-end | temp dirs + fake embedder/fake API |
| Golden | `tests/api_golden.rs` | live embedding vs frozen fixtures | online + key |
| Binary-level | manual full-chain regression before deploys | fetch→sync→MCP→report on release binaries | real environment |

Test discipline: deterministic fakes (content-hash vectors); zero network in CI; binary-level regression before shipping (unit greens lie across integration boundaries — proven twice).

## Commands

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings
cargo test --workspace            # unit + integration (golden excluded)
cargo run --bin gh-rag-mcp        # MCP serve (stdio)

gh-rag sync <repo> | --all        # full/incremental sync
gh-rag status / doctor / report / eval
gh-rag export --skeleton -o f     # skeleton export (distribution form)
gh-rag fetch --from <url|path>    # skeleton import (fingerprint-verified)

# Release: annotated tag required (lightweight tags are not pushed by --follow-tags)
git tag -a v0.x.y -m "..." && git push origin main v0.x.y
```

CI (`main`/`dev`): fmt + clippy + tests green before merge. Golden runs need a key (manual); ollama alignment is a manual workflow.

## Taboos

- ❌ Skipping or relaxing the golden threshold (0.999)
- ❌ Loading local models in core, or implicit global state beyond env vars
- ❌ Changing MCP signatures / CLI parameter shapes (frozen; additive return fields only)
- ❌ vec0 or any language-private virtual tables in index.sqlite
- ❌ Reintroducing Python / dual-language maintenance
- ❌ `pull_request_target` in any workflow (secrets-leak classic)
- ❌ English commit messages (this repo: Chinese, `<type>:<description>`, types = feat/fix/test/refactor/docs/ci/perf)
- ❌ Merging untested implementation changes
