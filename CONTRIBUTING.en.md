# Contributing

**[中文](CONTRIBUTING.md) | [English](CONTRIBUTING.en.md)**

Thanks for your interest in gh-rag! The project is in its validation phase with a deliberately constrained feature surface — **please open an issue to discuss new features before implementing**.

## Development Environment

- Rust stable (Windows / Linux / macOS)
- No model files or database services required; `cargo test --workspace` runs everything

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings
cargo test --workspace
```

CI gates (fmt / clippy `-D warnings` / tests) must be green to merge; `main` is branch-protected.

## Commit Convention

- **Commit messages in Chinese** (type prefixes and identifiers stay in English): `<type>:<description>`, types = feat/fix/test/refactor/docs/ci/perf
- Example: `fix:FTS 查询含括号时的语法错误`
- One commit, one thing; bug fixes come with a reproducing test

## Testing Discipline (TDD)

1. **Red → green → refactor**: failing test first, implementation second; bug fixes start with a test that reproduces the original bug
2. Deterministic fakes (content-hash-derived vectors, fake GitHub APIs); integration tests use zero network
3. The golden alignment test needs `GH_RAG_API_KEY` and is not part of regular CI:

```bash
GH_RAG_API_KEY=xxx cargo test -p gh-rag-core --features golden -- --nocapture
```

## Hard Constraints (violations = change is invalid; see AGENTS.md)

- **The golden threshold (0.999) is never relaxed**: it guards vector-space consistency; failures silently degrade retrieval quality
- **MCP tool signatures are frozen**: update the contract in AGENTS.md before changing code
- **One-way dependencies**: cli/mcp → core; core depends on no bin
- **Data layering**: GitHub API → raw layer → index; rebuilds go through raw only
- **No virtual tables** (vec0 or other language-private formats) in index.sqlite — cross-language compatibility is a design decision
- No `unwrap()` in core (tests excepted), no implicit global state

## Data Contributions

Index (skeleton) contributions go through [gh-rag-indexes](https://github.com/alex-1263/gh-rag-indexes): add a repo name via PR, or self-build in your fork. Skeletons contain vectors and factual metadata only — **never full text or comments** (copyright red line, CI-enforced).

## Code of Conduct

Be decent: critique code, not people; argue from evidence; when in doubt, let the data decide. Issue/PR text is copyrighted by its authors — do not redistribute it in bulk.
