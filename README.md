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

- [x] **M4 — Tier 3b: External Symbol Resolution**
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

- [ ] **M8 — Tier 3c: Semantic Diagnostics**
  Extend the `javac` integration from M5 into a background diagnostics source:
  invoke `javac` (or the build tool's compile task) per affected compilation unit,
  parse its diagnostic output, and republish as LSP diagnostics alongside the
  syntax/index-derived ones — real type errors (incompatible types, unresolved
  overloads, missing overrides), not just syntax and unresolved-symbol errors.
  *Done when:* an incompatible-types assignment in a testbed file produces a live
  diagnostic matching `javac`'s own message, refreshed precisely on the affected file
  without blocking editing.

- [ ] **M9 — Framework-Aware Navigation (Spring & Micronaut)**
  Recognize Spring's stereotype/DI annotations (`@Component`/`@Service`/
  `@Repository`/`@Controller`/`@RestController`/`@Configuration`/`@Bean`,
  `@Autowired`/`@Inject`-based injection) and Micronaut's compile-time DI
  (`@Singleton`/`@Inject` and its generated `*$Definition` classes, fed from M5's
  annotation-processing handling) to build a bean/implementation graph; extend
  go-to-definition/go-to-implementation so an injected field or constructor
  parameter resolves to its concrete bean(s), and add navigation between
  `@GetMapping`/`@RequestMapping`-style endpoint methods and their route metadata.
  *Done when:* in a testbed module with one interface and one `@Service`/`@Singleton`
  implementation, go-to-implementation on an injected field jumps to the concrete
  class, and a `@GetMapping` method exposes its route via hover or code lens.

- [ ] **M10 — Configuration File Intelligence**
  Parse `application.yml`/`application.properties` (including profile variants),
  and, using Spring's `spring-configuration-metadata.json` or Micronaut's equivalent
  metadata extracted from resolved dependency jars (M4), offer completion, hover
  documentation, and validation (unknown-key/type-mismatch diagnostics) for
  configuration keys.
  *Done when:* typing a known Spring Boot property key in the testbed's
  `application.yml` offers completion with its documented type and description, and
  an unknown key is flagged as a diagnostic.

- [ ] **M11 — On-Disk Cache & Cold Start**
  Persist the resolved project model (M3), parsed jar/JDK symbol tables (M4), and
  workspace index (M2) to an on-disk cache keyed by source/build-file content hashes;
  on reopening a project, hydrate from cache and re-resolve only what changed since
  the last session instead of rebuilding from scratch. Resolves the "Cache
  persistence" item in CLAUDE.md's Open Decisions.
  *Done when:* reopening Neovim on a large multi-module testbed-scale project reaches
  full completion/hover/diagnostics readiness in a small fraction of the original
  cold-start time, measured and committed as a benchmark alongside M6's.

- [ ] **M12 — Test Discovery & Run Integration**
  Recognize JUnit 5 test classes/methods, including Spring's `@SpringBootTest`/
  `@WebMvcTest`/`@DataJpaTest` slices and Micronaut's `@MicronautTest`; expose a code
  lens that shells out to the resolved build tool's test task (Gradle
  `test --tests`, Maven `-Dtest=`) to run a single test or class, streaming
  pass/fail back to the editor. Debugging is out of scope — that's DAP, a separate
  protocol this project doesn't implement.
  *Done when:* clicking "Run test" above a `@Test` method in the testbed runs just
  that test via the resolved build tool and reports pass/fail back to the editor.

- [ ] **M13 — Hover Documentation from Source & Javadoc**
  Resolve and parse the matching `-sources.jar` (or javadoc jar) for each dependency
  resolved in M4, so hover shows real Javadoc prose — not just signatures — for JDK
  and third-party/framework types.
  *Done when:* hovering a JDK type and a resolved third-party dependency type in the
  testbed shows their actual Javadoc text, not just the signature.

- [ ] **M14 — Modern Java Language Support**
  Verify and extend Tier 2 indexing and completion for records (implicit
  accessors/canonical constructor), sealed interfaces/classes, and pattern-matching
  `switch` (including record deconstruction patterns) — the syntax most Spring Boot
  3 / Micronaut 4 codebases (Java 17+) use for DTOs and request/response models.
  *Done when:* a record used as a Spring/Micronaut request/response DTO in the
  testbed gets correct completion for its accessor methods, and a pattern-matching
  `switch` over a sealed type in the testbed gets correct exhaustiveness-aware
  completion.

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
