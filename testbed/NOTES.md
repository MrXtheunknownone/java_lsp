# Testbed

A real Java project used to manually verify `java-lsp` against a real editor client,
per CLAUDE.md's Agentic Development & Review Process. Point an editor at this
directory with `java-lsp` configured as its Java language server.

Grows alongside the roadmap in `../README.md` — content is added exactly when the
milestone that needs it lands, not ahead of time:

- **M0/M1** (current): a single classpath-free file
  (`src/main/java/dev/javalsp/testbed/Main.java`), no build tool. Enough to verify
  the LSP handshake (M0) and syntax diagnostics (M1).
- **M2**: more files with declarations/imports/cross-references, to exercise the
  local workspace symbol index.
- **M3**: a build file (`build.gradle` or `pom.xml`), to exercise build tool
  detection and classpath resolution.
- **M4**: a real third-party dependency, to exercise external jar/JDK symbol
  resolution.
- **M5** (done): `Person` is `@Getter @Setter` (`lombok`, added to `build.gradle`);
  `Main.java` calls `person.getName()` so the generated getter has a real,
  navigable call site.
