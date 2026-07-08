# CLAUDE.md

Guidance for working on this project: a modern Java Language Server, implementing the
[Language Server Protocol](https://microsoft.github.io/language-server-protocol/), written in Rust,
for use with Neovim (and any other LSP-compliant client).

## Status

Early/greenfield. Architecture decisions not yet made — see "Open Decisions" below.
Do not assume a framework, crate layout, or dependency choice exists until it's decided
and recorded here.

## Architecture: Multistage Resolution Pipeline

The overriding non-functional goal of this project is **performance**: the user must
never notice expensive work happening. Every tier below is designed so that heavy work
runs in the background and interactive requests are served from whatever is already
cached, never by blocking on the next tier.

### Tier 1 — Syntax (in-memory, synchronous)

- Tree-sitter incremental parsing, updated on every edit.
- Produces immediate syntax diagnostics and a syntax tree usable by later tiers.
- The only tier allowed to run inline on the request path — it's fast enough to.

### Tier 2 — Local index (background, async)

- A fast, in-memory workspace symbol index built from tree-sitter ASTs: declarations,
  imports, and references within the project's own source.
- Powers completion/go-to-definition/hover for project-local code before external
  resolution is available.
- Built and updated incrementally in the background; a file edit re-indexes only the
  affected file(s), never the whole workspace.

### Tier 3 — External resolution (background, async, cached)

- Detect the build tool: Gradle (`build.gradle`/`build.gradle.kts`) or Maven
  (`pom.xml`), multi-module/multi-project aware (a project can have several source
  roots/classpaths, not just one).
- If a build tool is found: invoke it once (and again only when build files change) to
  resolve source roots, classpath, dependency jars, and Java version. The resolved
  project model is cached — the build tool is never re-invoked per query.
- If no build tool is found: fall back to plain `javac`. This path only supports
  classpath-free code (JDK-only); it cannot resolve external dependencies.
- Parse resolved dependency jars and JDK module classes for symbol info, feeding tier 2
  so external types resolve the same way local ones do.
- Annotation processors (notably Lombok) are handled as part of this tier — their
  generated members must become visible to the index, not just silently unresolved.

### Never-blocks rule

If tier 3 (or tier 2) hasn't finished for a given piece of code, requests are answered
from whatever tier is ready — best-effort, not blocked. When a slower tier finishes,
affected results are refreshed (e.g. diagnostics republished) rather than the original
request being held open.

### Dependency minimalism applies here too

The heavier pieces of this architecture — build tool invocation, jar/classfile
parsing, the tree-sitter grammar — are exactly where it's tempting to reach for a big
dependency. The project-wide rule in "Dependencies" below still applies: prefer the
standard library and small, well-maintained crates; every crate used to implement a
tier must be justifiable in a sentence.

## Performance & Concurrency

- Heavy work (indexing, build tool invocation, jar/classfile parsing, annotation
  processing) always runs off the request-handling path — background tasks, never
  inline in a `textDocument/*` handler.
- A slow or unresolved external dependency must never freeze completion, hover, or
  diagnostics for code that doesn't depend on it.
- Cache aggressively at every tier; invalidate precisely (only the affected
  files/modules) rather than globally, on change.
- Surface background work to the client via LSP progress reporting
  (`window/workDoneProgress`) instead of making the client wait silently.

## Test-Driven Development

- No unit is implemented before its test. Write a failing test first, then the minimum
  code to pass it, then refactor.
- "Unit" means the smallest independently-testable piece of behavior — typically a
  function or small module, not just public API surface. Internal logic gets tests too.
- Tests live alongside the code they test (`#[cfg(test)] mod tests` in the same file),
  unless testing cross-module/integration behavior, which goes under `tests/`.
- A change is not done until: tests are written, the new code passes them, and the
  full suite still passes.
- Do not write implementation code speculatively "to see if it works" and backfill
  tests afterward — that isn't TDD and defeats the purpose (tests only proven to pass
  against code they were written to justify).

## Code Style

- Clean code means readable code. If it needs a comment to explain what it does,
  rewrite it — better names, smaller functions, clearer structure — until it doesn't.
- No inline comments explaining *what* code does.
- `///` rustdoc on public API is fine (it documents a contract for callers, not the
  implementation) — but keep it factual and minimal, not a substitute for a clear
  signature.
- Prefer clarity over cleverness. A slightly longer, obvious implementation beats a
  compact, opaque one.
- No dead code, no commented-out code, no speculative abstractions for hypothetical
  future needs. Three similar lines beat a premature abstraction.
- `rustfmt` and `clippy` (default lint set, deny warnings) are non-negotiable gates —
  code that doesn't pass both isn't done.
- Errors are values: use `Result`, avoid `panic!`/`unwrap`/`expect` outside of tests
  and truly unreachable invariants. No silent failure — an error is surfaced, not
  swallowed.

## Scope Discipline

- Implement what the current task requires, not what might be useful later.
- No configuration flags, extension points, or feature toggles without a concrete
  current use.
- When the LSP spec is ambiguous or offers optional behavior, implement the minimal
  compliant behavior first; extend only when a real client/scenario needs more.

## Dependencies

- Prefer the standard library and a small number of well-maintained crates over
  reinventing infrastructure (e.g. JSON-RPC framing), but don't pull in a crate for
  something trivial to write and test directly.
- Every new dependency should be justifiable in a sentence: what it replaces, why it's
  safer/faster to depend on it than to write it.

## Commits

- Each commit should represent one coherent, working change (tests included) — avoid
  bundling unrelated fixes.
- Commit messages describe *why*, not a restatement of the diff.

## Agentic Development & Review Process

This project is built milestone by milestone — see the Roadmap in `README.md` for the
current milestone and its "done when" criteria. Within a milestone, work proceeds in
small units, each following the TDD cycle above.

No unit is considered done on tests passing alone:

1. Write the failing test, then the minimal implementation, then refactor (per TDD).
2. Run `rustfmt` and `clippy` — both clean.
3. Run a `/code-review` pass on the change — catch correctness bugs and
   simplification/reuse opportunities a quick skim would miss.
4. For anything with observable runtime behavior (an LSP capability, a diagnostic, a
   completion result) — run `/verify` or otherwise exercise it end-to-end against a
   real client, not just unit tests. Passing tests confirm the code does what the test
   says; verification confirms the test said the right thing. Use `testbed/` (a
   sample Java project in this repo) as the target for this — grow its content
   exactly when the milestone under test needs the new capability (a build file, a
   dependency, a Lombok-annotated class, etc.), not ahead of time, per Scope
   Discipline below.

A milestone is not done until every unit in it clears the above and the milestone's
"done when" criterion in `README.md` is demonstrably true.

### Git & GitHub stay with the developer

Agents never commit, push, or otherwise touch git or GitHub (no `git commit`, no
`git push`, no opening/commenting on PRs or issues) — not even after a clean
review/verify pass. Every change is left as working-tree edits for a developer to
review and commit themselves. Updating the roadmap checkbox in `README.md` is part of
the change, but staging/committing it is not the agent's job.

## Open Decisions

Settled: parsing (tree-sitter), build tool support (Gradle + Maven, `javac` fallback),
the multistage async architecture above, and (as of M0) the following:

- Async runtime: **tokio**.
- LSP transport/framework: **hand-rolled JSON-RPC** framing and dispatch (Content-Length
  header framing over stdio) — not `tower-lsp`, not `lsp-server`. LSP protocol structs
  themselves (`InitializeParams`, `ServerCapabilities`, `Diagnostic`, etc.) use the
  **`lsp-types`** crate rather than being hand-rolled — that schema is large and revs
  independently of this project, which is exactly the kind of infrastructure the
  Dependencies section below says to prefer a crate for; the framing/dispatch loop is
  small and specific enough to us that hand-rolling it is reasonable.
- Crate layout: **single crate**. No workspace split yet — module boundaries between
  tiers aren't concrete until later milestones (e.g. M2/M3) give them shape.

Still open — do not silently pick one while implementing; raise it for a decision,
then update this section:

- Cache persistence: in-memory only (rebuilt each session) vs. on-disk cache to speed
  up reopening large projects.
