# Java LSP

A modern, fast [Language Server Protocol](https://microsoft.github.io/language-server-protocol/)
implementation for Java, written in Rust, built for Neovim primarily.

## Status

Early development — pre-alpha, not yet usable. See [Roadmap](#roadmap) for progress.

## Why

Existing Java language servers are heavy and slow to respond under load. This
project's primary goal is interactive performance: expensive work like project
indexing, build tool resolution, and dependency jar parsing must never block the
editor. Every feature is built to run that work in the background and answer requests
from whatever is already cached.

## Architecture

The server resolves code in three stages, each feeding the next:

1. **Syntax** — incremental parsing (tree-sitter) on every edit, producing instant
   syntax diagnostics.
2. **Local index** — a fast, in-memory symbol index built from the parsed source,
   powering completion, go-to-definition, and hover for code within the workspace.
3. **External resolution** — classpath and dependency resolution via the project's
   build tool (Gradle or Maven), or `javac` when no build tool is present; resolved
   once and cached rather than re-invoked per request.

Heavier work always happens asynchronously in the background; interactive requests are
answered from whatever's already resolved, never blocked on a slower stage.

## Roadmap

Milestones are ordered bottom-up through the architecture's stages above; each is done
only when its criterion below is demonstrably true, tested, and verified end-to-end.

- [x] **M0 — Project Scaffolding & LSP Handshake**
  Cargo project set up, CI running fmt/clippy/test. Server speaks the LSP transport
  over stdio and completes `initialize`/`initialized`/`shutdown`/`exit` with a real
  client.
  *Done when:* Neovim attaches to the server and receives capabilities without
  crashing.

- [x] **M1 — Tier 1: Syntax**
  tree-sitter-java wired in with incremental parsing on `didOpen`/`didChange`/
  `didClose`; syntax errors published as diagnostics.
  *Done when:* a syntax typo in an open Java file shows a live diagnostic in Neovim.

- [x] **M2 — Tier 2: Local Index**
  Workspace symbol index built from ASTs (declarations, imports, references within the
  project's own source); completion, go-to-definition, and hover for project-local
  symbols.
  *Done when:* navigating/completing symbols defined in the same workspace works, with
  no external dependency resolved yet.

- [x] **M3 — Tier 3a: Build Tool Bootstrap**
  Detect Gradle/Maven (multi-module aware), invoke once to resolve source roots,
  classpath, dependency jars, and Java version; cache the resolved project model.
  *Done when:* the resolved classpath for a real Gradle sample project and a real
  Maven sample project is captured and cached correctly, verified by test fixtures.

- [ ] **M4 — Tier 3b: External Symbol Resolution**
  Parse dependency jars and JDK module classes for symbol info, feeding Tier 2.
  *Done when:* completion/hover/go-to-definition work for JDK types (e.g.
  `java.util.List`) and third-party library types.

- [ ] **M5 — javac Fallback & Annotation Processing**
  `javac`-backed fallback for build-tool-less, classpath-free code; annotation
  processor (notably Lombok) generated members made visible to the index.
  *Done when:* a Lombok-annotated class resolves its generated getters/setters
  correctly.

- [ ] **M6 — Performance Hardening**
  Precise cache invalidation, background task scheduling audit, `workDoneProgress`
  reporting; benchmarks against a large real-world codebase.
  *Done when:* editing in a large project shows no perceptible input lag, backed by
  benchmark numbers committed to the repo.

- [ ] **M7 — LSP Feature Completeness & Distribution**
  Remaining capabilities as needed (find references, rename, code actions,
  formatting, semantic tokens); documented Neovim setup.
  *Done when:* a documented Neovim config installs and uses the server for day-to-day
  Java editing.

## Development

Requirements: contributions follow test-driven development — every unit of behavior
is implemented test-first. Code must pass `cargo fmt` and `cargo clippy` cleanly, and
read clearly enough that it doesn't need comments to explain what it does.

Build with `cargo build`; run the full test suite with `cargo test`. The server only
speaks LSP over stdio, so a real client is needed to exercise it manually — a minimal
Java project lives in [`testbed/`](./testbed) for exactly this: point an editor at it
with `java-lsp` configured as its Java language server to manually verify a milestone
end-to-end (see `CLAUDE.md`'s Agentic Development & Review Process). `testbed/` grows
alongside the roadmap above — it starts as a single classpath-free file and gains a
build file, a real dependency, and Lombok usage as the milestones that need them
land.

## License

MIT — see [`LICENSE`](./LICENSE).
