# Roadmap

This is a living document. Dates are intentionally absent — `jet` ships milestones, not deadlines.

Legend: `[ ]` not started · `[~]` in progress · `[x]` done

---

## 0.1 — "It compiles" 🛠️

The smallest thing that resembles a build tool. No network, no dependencies — just the JDK on your `PATH`.

- [~] CLI scaffold (`clap`-based) with all top-level subcommands stubbed
- [ ] `jet new <name>` — generate a project with `jet.toml`, `src/main/java/Main.java`, `.gitignore`
- [ ] `jet.toml` parser (`[package]`, `java`, `[dependencies]`)
- [ ] `jet build` — invoke `javac` against `src/main/java`, output to `target/classes`
- [ ] `jet run` — execute `Main` (or `[package].main`) via `java -cp`
- [ ] `jet clean` — `rm -rf target/`
- [ ] Source-level incremental compilation (skip unchanged files by mtime + hash)
- [ ] Helpful error messages when `javac`/`java` are missing

**Exit criteria:** A user can `jet new && jet run` and see `Hello, world!` in under a second after the first compile.

## 0.2 — "It downloads" 📦

Talk to Maven Central. The first version that competes with `mvn` for non-trivial projects.

- [ ] Maven coordinate parsing (`group:artifact:version[:classifier]`)
- [ ] HTTP fetcher with on-disk cache (`~/.jet/cache/`)
- [ ] POM parser (just enough: `dependencies`, `parent`, `dependencyManagement`, properties)
- [ ] Transitive resolution with conflict resolution (nearest-wins, like Maven)
- [ ] `jet.lock` lockfile — generate, validate, refuse to build if drifted
- [ ] `jet add <coord>` — append to `jet.toml` and re-resolve
- [ ] Concurrent downloads with progress bars
- [ ] `[repositories]` table for non-Central repositories (incl. auth)

**Exit criteria:** A project depending on `jackson-databind` and `slf4j-api` builds against the same classpath Maven would produce, and reruns are cache-hits.

## 0.3 — "It tests" ✅

- [ ] `src/test/java` compiled separately with the test classpath
- [ ] JUnit 5 (`junit-jupiter`) auto-detected and wired up
- [ ] `jet test [filter]` — discovery + execution + colored output
- [ ] Test report (console + JUnit XML for CI)
- [ ] `[dev-dependencies]` table

**Exit criteria:** A project with JUnit 5 tests runs them with `jet test` and exits non-zero on failure.

## 0.4 — "It packages" 🎁

- [ ] `jet package` — produce `target/<name>-<version>.jar`
- [ ] Manifest generation (`Main-Class`, `Implementation-Version`)
- [ ] `--uber` / shaded jar with conflict detection
- [ ] Resource handling (`src/main/resources`)
- [ ] Reproducible jars (sorted entries, fixed timestamps)

**Exit criteria:** `java -jar target/foo-0.1.0.jar` runs the application end-to-end.

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
