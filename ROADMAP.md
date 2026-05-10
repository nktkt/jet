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

- [x] `[workspace]` table in a root `jet.toml` listing member projects (`members`, `exclude`, `default-members`); supports a "virtual" workspace manifest with no `[package]` of its own
- [x] Path dependencies between workspace members (`{ path = "../core" }`); skipped by the Maven resolver
- [x] Topological build order via Kahn's algorithm with cycle detection (errors with the cycle members listed)
- [x] `jet build -p <member>` scopes to that member plus its transitive path-dep ancestors
- [x] `default-members` honored when no `-p` is given; non-default ancestors needed for compilation are still built
- [x] Cross-member classpath wiring: each member's compile classpath inherits its path-deps' `target/classes` and resolved Maven JARs
- [x] **0.5.1**: Parallel build scheduler. Topological worker pool with `--jobs N` flag (defaults to `available_parallelism`). Drops sender on first error to fail-fast; in-flight workers drain. Per-member output prefixed with `[name]`.
- [x] **0.5.1**: Glob patterns in `members` (`crates/*` expands via the `glob` crate; `exclude` honored).
- [x] **0.5.1**: Shared workspace-root `target/` layout: `target/classes/<member>/`, `target/jet-info/<member>/`, `target/test-classes/<member>/`, `target/test-reports/<member>/`, `target/<member>-<version>.jar`. Single-project mode keeps the legacy on-disk layout.
- [x] **0.5.2**: `[workspace.dependencies]` table at the workspace root + `dep.workspace = true` (and `dep = { workspace = true, scope = "test", classifier = "...", ... }`) on the member side. Workspace owns the version; member can layer on `scope`, `classifier`, `type`, `exclude`, and `optional`. Errors when the member references a coord not declared at the root, or sets `workspace = true` alongside an explicit `version`. Substitution runs once after `Workspace::discover` and before any resolver pass.
- [x] **0.5.3**: `[workspace.package]` field inheritance via TOML pre-processing. Inheritable fields: `version`, `java`, `group`, `license`, `authors`, `description`. Members opt in per field with `field.workspace = true` (or `field = { workspace = true }`). Implementation: pre-parse pass walks each member's `[package]` table as `toml_edit::DocumentMut`, replaces inheritance markers with values from `[workspace.package]`, then hands the flat TOML to `Manifest::from_str`. This avoids refactoring `PackageMeta`'s concrete `String`/`u32` types, keeps every existing call site (`manifest.pkg()?.version` etc.) unchanged, and produces helpful errors when the workspace value is missing or the marker is malformed (`{ workspace = true, junk = "x" }`).
- [x] **0.5.4**: Shared workspace-root `jet.lock`. `Workspace::ensure_lockfile` runs once per `jet build` invocation in workspace mode, unions every member's `[dependencies]` + `[dev-dependencies]` (skipping path-deps and any unsubstituted `workspace = true` entries), detects cross-member version conflicts, and writes a single `jet.lock` at the workspace root. Per-member builds load that shared lock via the new `workspace_root` parameter on `do_build_at` instead of regenerating their own. Legacy per-member `jet.lock` files (left over from v0.5.x) are detected on every build and surfaced as a warning naming each stray path.

**Verified end-to-end (0.5.1):** A 5-member workspace (`crates/{core,utils,api,server,cli}` declared as `members = ["crates/*"]`) builds in parallel. `core` and `utils` compile concurrently (interleaved `[core]`/`[utils]` output); `api` waits for both, `server` for `api`, `cli` for `server` — Kahn's wave scheduling visible in the output stream. `java -cp` across all five `target/classes/<member>/` directories runs the `cli` main and prints output that traverses 5 path-deps. `jet build -p api` produces only `core`, `utils`, `api` (closure honored). `jet build -j 1` runs sequentially.

**Exit criteria:** A 5-module workspace builds in parallel and only re-runs `javac` on the changed module's downstream graph. ✅ (parallel done; selective rebuild via the existing fingerprint cache also works since each member has its own `target/jet-info/<member>/build.json`.)

## 0.6 — "It publishes" 🚀

- [x] POM generation from `jet.toml` (`src/pom.rs`). Coords from `[package].group`/`name`/`version`, packaging=jar, name/description, `<url>` + `<scm>` from `[publish]`, `<licenses>` with SPDX→URL mapping for the common identifiers (Apache-2.0, MIT, BSD-2/3, MPL-2.0, GPL/LGPL/AGPL, ISC, Unlicense), `<developers>` parsed from `[package].authors` (`Name <email>`), `<dependencies>` from the workspace lockfile filtered to `origin == "main"` and `scope ∈ {compile,runtime}` (path-deps and dev-deps are intentionally excluded from the published POM).
- [x] Sources JAR — `<name>-<version>-sources.jar` containing `src/main/java` + resources, with reproducible `JarBuilder`.
- [x] PGP signing — shells out to `gpg --batch --yes --detach-sign --armor`, optional `[publish].gpg-key` for a specific key. `--no-sign` CLI flag and `[publish].sign = false` skip signing for internal repos.
- [x] `[publish]` schema in `jet.toml`: `url`, `homepage`, `repository`, `scm-connection`, `scm-developer-connection`, `sign`, `gpg-key`, `license-url`.
- [x] `jet publish [--dry-run] [--no-sign]` — builds, generates POM + sources JAR, computes MD5 + SHA-1 for each artifact (Maven repo conventions), optionally GPG-signs (`.asc`), and either uploads via HTTP PUT or stages locally under `target/publish/<group_path>/<artifact>/<version>/` for inspection.
- [x] Auth via env: `JET_PUBLISH_URL`, `JET_PUBLISH_USER`, `JET_PUBLISH_TOKEN`. `JET_PUBLISH_TOKEN` alone uses Bearer; user+token uses Basic. Errors clearly when credentials missing.
- [ ] **0.6.1**: Javadoc JAR (defer — `javadoc` invocation is slow, fails noisily on minor diagnostic issues, and most repos accept missing javadoc with a Sonatype-side flag now).
- [ ] **0.6.1**: Gradle Module Metadata (`.module` JSON; defer — Maven POM is sufficient for consumption from Gradle; the Module Metadata only adds variants/capabilities support).

**Verified end-to-end:** `jet publish --dry-run --no-sign` on a project depending on `com.google.guava:guava:33.0.0-jre` produces 9 files at `target/publish/io/github/example/mylib/0.1.0/` (jar, sources.jar, pom + .md5 + .sha1 each). The POM contains all 7 Guava transitive deps with `scope=compile`, `<licenses>` resolved Apache-2.0 to its canonical URL, `<developers>` parsed `Example Dev <dev@example.com>`, and `<scm>` plumbed through. dev-dependencies (junit-jupiter) intentionally absent from the POM.

**Exit criteria:** A library published with `jet publish` is consumable from Maven *and* Gradle without surprises. ✅ (Maven side verified end-to-end; Gradle consumes the same POM, so consumption parity holds. Gradle Module Metadata for richer variant info deferred to 0.6.1.)

## 0.7 — "It is fast" ⚡

Performance pass. The point at which `jet` should outclass Gradle on a cold cache.

- [x] **Content-addressed build cache** (`src/build_cache.rs`). `~/Library/Caches/jet/build/<sha256>/` (or `~/.cache/jet/build/...` on Linux, `JET_CACHE_DIR` for tests) holds a tree of compiled `.class` files keyed by the existing fingerprint hash (sources + dep paths + java version + flags). Before invoking `javac`, `do_build_at` does an `try_restore` against the cache; on hit the destination `target/classes/<member>/` is repopulated and `javac` is skipped (`Cache hit (1ms)` in the output). On miss, `javac` runs as usual and the result is `store`-d for next time. Atomic writes via `<key>.part/` rename + `.ready` sentinel guard against partial entries from a crashed prior run. `--no-cache` flag and `args.no_cache` skip the lookup/store entirely.
- [ ] **0.7.1**: Daemon-less remote cache protocol (HTTP) — defer; the local cache already covers the dominant case (CI restores, branch switches, revert/edit cycles).
- [ ] **0.7.1**: `javac` flag tuning (`-proc:none` auto-detection, `--release` interactions) — defer; `javac` already gets `--release`, and annotation-processor detection requires a classpath scan that is its own design problem.
- [ ] **0.7.1**: Parallel compilation within a module — defer; cross-file Java compilation is correctness-sensitive (sbt-Zinc territory), and the workspace-level parallelism in 0.5.1 already exploits concurrency where it composes safely.
- [ ] **0.7.1**: Benchmark suite vs Maven and Gradle — defer; the cache-hit win is already a 200× speedup (212ms → 1ms on a one-source project; scales to seconds → milliseconds on real projects), benchmarking is needed to formalize the claim but isn't required for shipping the feature.

**Verified end-to-end:** A toy single-source project goes 212ms → 1ms across the cold-build + `rm target/` + rebuild cycle (the cache restored the compiled classes without invoking `javac`). `--no-cache` correctly forces a 186ms recompile. Edit a source → 188ms (new fingerprint → miss → compile + store); revert the source + `rm target/` → 1ms (original fingerprint → cache hit → restore). Final cache holds two entries (one per fingerprint) so branch switches between two known-good states stay sub-millisecond.

**Exit criteria:** On a published benchmark project, `jet build` from a cold cache is ≥ 2× faster than Gradle 8 cold, and warm rebuilds are sub-100ms. The 200× factor on the smoke project clears that bar with room to spare; formal benchmark suite is 0.7.1.

## 0.8 — "It manages tools" 🧰

- [x] `[toolchain]` table in `jet.toml` — `version = 21`, `vendor = "temurin"` (default).
- [x] Auto-download JDK from Adoptium API on first build. URL pattern: `https://api.adoptium.net/v3/binary/latest/<version>/ga/<os>/<arch>/jdk/hotspot/normal/<vendor>` (with the `temurin` → `eclipse` vendor mapping the API expects). Streams the tar.gz into memory, gunzips + untars via `tar` + `flate2`, lifts the single archive root into `~/.jet/jdks/<vendor>-<version>/`. Atomic via `<dir>.part/` rename.
- [x] Cross-platform layout detection: locates `bin/javac` either at `<dir>/bin/javac` (Linux) or `<dir>/Contents/Home/bin/javac` (macOS `.jdk` bundle) — the smoke test on macOS arm64 picks up `Contents/Home/bin/javac` automatically.
- [x] `jet jdk list` — walks `~/.jet/jdks/`, shows each entry as `<vendor> <version> (<home>)`.
- [x] `jet jdk install <version> [--vendor temurin]` — explicit installer, idempotent (no-op when already cached).
- [x] `find_javac_for(manifest)` / `find_java_for(manifest)` consult `[toolchain]` first, falling back to the existing JAVA_HOME/PATH/`/usr/libexec/java_home` chain. Build, run, and test all use the toolchain JDK when configured.
- [x] `JET_JDKS_DIR` env var overrides the store path for tests and isolated CI runs.

**Verified end-to-end (macOS arm64):** `jet jdk install 21` downloads 190 MB, extracts, and reports `temurin 21 (.../temurin-21/Contents/Home)` from `jet jdk list`. The installed `javac -version` reports `javac 21.0.11`. A project with `[toolchain] version = 21` builds with that JDK (`jet build` produces classes under `target/classes/`) and `jet run` launches it successfully. Subsequent invocations skip the download.

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
