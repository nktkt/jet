# Roadmap

This is a living document. Dates are intentionally absent — `jet` ships milestones, not deadlines.

Legend: `[ ]` not started · `[~]` in progress · `[x]` done

---

## 0.1 — "It compiles" 🛠️

The smallest thing that resembles a build tool. No network, no dependencies — just the JDK on your `PATH`.

- [x] CLI scaffold (`clap`-based) with all top-level subcommands stubbed
- [x] `jet new <path>` — generate a project with `jet.toml`, `src/main/java/<pkg>/Main.java`, `.gitignore`, `src/main/resources/.gitkeep`
- [x] `jet init` — same scaffold in the current directory, non-destructive (skips files that already exist)
- [x] `--no-vcs` flag; default runs `git init --quiet`
- [x] Project name validation (length, charset, Java keywords, Windows-reserved, build-tool reserved)
- [x] Hyphen-aware Java package derivation (`my-app` → `com.example.my_app`)
- [x] `jet.toml` parser (`[package]`, `[dependencies]`, `[dev-dependencies]`, `[repositories]`, `[build]`) via serde + toml_edit
- [x] `jet build` — invoke `javac` against `src/main/java`, output to `target/classes`
- [x] `jet run` — execute `Main` (or `[package].main`) via `java -cp`; auto-scans `target/classes` for `public static void main` if not declared
- [x] `jet clean` — `rm -rf target/`
- [x] Coarse incremental compilation — sha256 fingerprint of (sources + deps + java version + flags); full recompile on any change
- [x] Helpful error messages when `javac`/`java` are missing — JAVA_HOME state echoed, links to SDKMAN/Adoptium

**Exit criteria:** A user can `jet new && jet run` and see `Hello, world!` in under a second after the first compile.

## 0.2 — "It downloads" 📦

Talk to Maven Central. The first version that competes with `mvn` for non-trivial projects.

- [x] Maven coordinate parsing (`group:artifact:version[:classifier][@type]`) — `src/coord.rs`
- [x] HTTP fetcher (`ureq`) with on-disk cache (`~/Library/Caches/jet/` etc. via the `dirs` crate)
- [x] POM parser (`quick-xml` streaming) — `dependencies`, `parent`, `dependencyManagement`, properties, exclusions, optional, BOM imports
- [x] Property interpolation (`${project.version}`, `${project.groupId}`, user-defined)
- [x] Parent POM chain resolution (bounded to 32 hops)
- [x] BOM imports (`<type>pom</type><scope>import</scope>`) — fetched and merged into dependencyManagement
- [x] Transitive resolution with **nearest-wins** (Maven default): shallowest depth, first-declared as tiebreaker
- [x] Scope handling: skip `test`/`provided`/`system` transitively; `import` scope only contributes management
- [x] Optional dependencies skipped transitively
- [x] Per-edge exclusions, inherited by subtree
- [x] Version range syntax rejected with a clear error (deferred to a later release)
- [x] `jet.lock` lockfile — TOML, sorted, atomic write, mirrors `Cargo.lock` conventions
- [x] `jet add <group:artifact:version>` — validates coord, HEAD-checks Maven Central, mutates `jet.toml` via `toml_edit` (preserves comments), regenerates `jet.lock`
- [x] sha256 verification of cached artifacts (refetches on mismatch)
- [x] `[repositories]` schema in `jet.toml` (resolved by declaration order)
- [ ] Concurrent downloads with progress bars (currently sequential)
- [ ] Authenticated repositories (Sonatype/GitHub Packages credentials)
- [ ] SNAPSHOT versions (deferred to 0.3)

**Verified end-to-end:** `jet add com.google.guava:guava:33.0.0-jre` resolves 7 transitive dependencies, fetches their JARs, compiles a Java program using `ImmutableList`, and runs it.

**Exit criteria:** A project depending on `jackson-databind` and `slf4j-api` builds against the same classpath Maven would produce, and reruns are cache-hits. ✅

## 0.3 — "It tests" ✅

- [x] `src/test/java` compiled separately with a test-only classpath (main classes + main deps + dev deps)
- [x] JUnit 5 (`junit-jupiter`) — Jupiter aggregate auto-resolved from `[dev-dependencies]`; junit-platform-console-standalone fetched on-demand and cached
- [x] `jet test [filter]` — `--details=tree` colored output streamed verbatim
- [x] Filter dispatch: `Foo::bar` (or `Foo#bar`) → `--select-method`; `com.pkg.Foo` → `--select-class`; `com.pkg.*` → `--select-package`; lowercase → substring `--include-classname`
- [x] JUnit XML report written to `target/test-reports/` (Surefire-compatible — `TEST-junit-jupiter.xml` etc.)
- [x] `[dev-dependencies]` table — single-pass resolution with `origin` tag (`main` / `dev`); main wins on overlap; persisted in jet.lock
- [x] `jet add --dev <coord>` — adds to `[dev-dependencies]`
- [x] `jet new` scaffolds a sample test (`src/test/java/<pkg>/MainTest.java`) and pre-fills the JUnit Jupiter dev-dep
- [x] `-parameters` added to test compile (JUnit reflection); `-Werror` removed for tests
- [x] Friendly error when no test framework is declared, pointing at `jet add --dev`
- [x] Non-zero exit code on test failure (verified: `jet test` exits 1 on a failing assertion)

**Verified end-to-end:** `jet new app && jet test` resolves the full JUnit Jupiter graph (8 transitive deps), downloads the JUnit Platform Console Launcher (1.10.2), compiles the scaffolded test, runs it with tree-style output, and writes Surefire-compatible XML reports. A failing test produces exit code 1.

**Exit criteria:** A project with JUnit 5 tests runs them with `jet test` and exits non-zero on failure. ✅

## 0.4 — "It packages" 🎁

- [x] `jet package` — produces `target/<name>-<version>.jar` (thin), `target/<name>-<version>-uber.jar` (with `--uber`)
- [x] `MANIFEST.MF` generation (`Manifest-Version`, `Created-By: jet <ver>`, `Build-Jdk-Spec`, `Implementation-Title/Version/Vendor`, `Main-Class` when executable, 72-byte line wrap with leading-space continuation)
- [x] Main-Class detection: `[package].main` → scan `target/classes` → fallback to library JAR (no `Main-Class` header) on zero or multiple candidates
- [x] `--uber`: bundles all main `[dependencies]` JAR contents
  - Class dedupe by content hash; identical bytes silently merged, differing bytes warned (kept earlier source)
  - `META-INF/services/*` line-merged (concatenated, deduped) — preserves ServiceLoader providers
  - `META-INF/*.SF`, `*.RSA`, `*.DSA`, `*.EC`, `SIG-*` stripped (signatures cannot survive shading)
  - `META-INF/MANIFEST.MF` from deps skipped; jet writes its own
  - `META-INF/LICENSE*` and `META-INF/NOTICE*` renamed to `META-INF/<name>-<jar>` to preserve attribution
  - `module-info.class` and `META-INF/versions/*` skipped (TODO 0.5: MR-JAR support)
  - Project's classes win over deps; warnings collected and printed at end
- [x] Resource handling: `src/main/resources` walked at package time (no copy to `target/classes`); compiled output wins on path conflict; default-excludes `.DS_Store`, `Thumbs.db`, `*.swp`, `*~`, etc.
- [x] **Reproducible JARs**: entries sorted lexicographically (POSIX byte order), `META-INF/` and `MANIFEST.MF` hoisted to positions 0/1; `SOURCE_DATE_EPOCH` honored (clamped to 1980-01-01 DOS minimum), default `2024-01-01T00:00:00Z`; fixed Unix mode `0o644` files / `0o755` dirs; deflate compression with default settings

**Verified end-to-end:**
- Thin JAR: `jet new app && jet package` → 4 entries, 875 bytes, `java -jar` runs.
- Uber JAR: project + Guava (7 transitive deps) → 2486 entries, 3.1MB, `java -jar` runs and prints output using `ImmutableList`.
- **Reproducibility**: two consecutive `jet package --uber` builds produce byte-identical SHA-256 (`c387904c…4300`).

**Exit criteria:** `java -jar target/foo-0.1.0.jar` runs the application end-to-end. ✅

## 0.5 — "It scales" 🏗️

Workspaces. Cargo's killer feature, ported to the JVM.

- [ ] `[workspace]` table in a root `jet.toml` listing member projects
- [ ] Path dependencies between workspace members
- [ ] Topological build order, parallelized
- [ ] Shared `target/` and lockfile across the workspace
- [ ] `jet build -p <member>` to scope work

**Exit criteria:** A 5-module workspace builds in parallel and only re-runs `javac` on the changed module's downstream graph.

## 0.6 — "It publishes" 🚀

- [ ] POM generation from `jet.toml` (with `[publish]` metadata)
- [ ] Sources jar + Javadoc jar
- [ ] PGP signing (delegating to `gpg`)
- [ ] `jet publish` to Maven Central staging / Sonatype / GitHub Packages
- [ ] Gradle Module Metadata emission (so Gradle consumers get the same view)

**Exit criteria:** A library published with `jet publish` is consumable from Maven *and* Gradle without surprises.

## 0.7 — "It is fast" ⚡

Performance pass. The point at which `jet` should outclass Gradle on a cold cache.

- [ ] Content-addressed build cache (file → output hash)
- [ ] Daemon-less remote cache protocol (HTTP, optional)
- [ ] `javac` flag tuning + `--release` handling
- [ ] Parallel compilation within a module (file partitioning)
- [ ] Benchmark suite vs Maven and Gradle on representative projects

**Exit criteria:** On a published benchmark project, `jet build` from a cold cache is ≥ 2× faster than Gradle 8 cold, and warm rebuilds are sub-100ms.

## 0.8 — "It manages tools" 🧰

- [ ] `[toolchain]` table — declare required JDK version
- [ ] Auto-download JDK from Adoptium / Foojay if missing
- [ ] `jet jdk list` / `jet jdk install <version>`

## 0.9 — "It extends" 🔌

- [ ] Plugin API design (out-of-process by default; no classpath pollution)
- [ ] First built-in plugins: `spring-boot`, `shadow`, `protobuf`
- [ ] Plugin discovery via Maven coordinates
- [ ] `jet why <coord>` and `jet tree` for diagnostics

## 1.0 — "It is stable" 🎯

- [ ] `jet.toml` schema frozen with versioning (`edition = "2026"`)
- [ ] Plugin API stable
- [ ] Migration guide from Maven and Gradle, with importer (`jet import`)
- [ ] Documented release cadence and deprecation policy

---

## Beyond 1.0 (sketches)

- **Watch mode** — `jet watch` recompiles + reruns tests on save (Bun-style).
- **Remote build cache** — shared S3-backed cache, à la Bazel.
- **Native image** — first-class GraalVM support.
- **Kotlin / Scala** — second-class but workable, via the existing toolchain hook.
- **`jet doctor`** — diagnose project misconfigurations.

## Open questions

These are explicitly undecided. Opinions welcome via issues.

1. **Build script escape hatch.** Cargo has `build.rs`. Should `jet` have `build.java`, a TOML hook table, or refuse entirely and force plugins?
2. **Version selection.** Maven's nearest-wins vs Gradle's highest-wins vs strict (lockfile-only) — pick one default.
3. **Manifest extension model.** Strict schema (reject unknown keys) vs permissive (forward-compatible)?
4. **Test framework neutrality.** Hard-wire JUnit 5, or pluggable from day one?
5. **Daemon.** Sworn off, or grudgingly accepted for sub-100ms warm builds?
