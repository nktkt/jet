# Changelog

All notable changes to `jet` are documented here. The format loosely follows
[Keep a Changelog](https://keepachangelog.com/). jet adheres to
[Semantic Versioning](https://semver.org/) from `1.0.0` onward.

## [1.1.0] — 2026-05-11

### Added

- **`jet watch [build|test]`** — recursively watches `src/main/java`,
  `src/main/resources`, `src/test/java`, `src/test/resources`, and
  `jet.toml`, debounces editor save bursts to 200 ms, and re-runs the
  chosen command on each batch. Ignores `target/`, `.git/`, `.jet/`,
  and `node_modules/`. Defaults to `build`; `jet watch test` for the
  red-green loop. Runs until Ctrl-C.
- Combined with the 0.7 content-addressed cache, warm rebuilds in
  `jet watch` measure 1 ms when nothing semantic changed; the hot path
  (single source edit) lands at ~210 ms — the time `javac` itself
  takes. A burst of three rapid edits coalesces into one rebuild.

## [1.0.0] — 2026-05-11

### Stability promise

- **Manifest schema is frozen.** `[package].edition = "2026"` pins the schema;
  future jet versions add new editions but continue to read `"2026"`
  manifests until at least one major release after deprecation.
- **CLI is stable.** Built-in subcommands (`new`, `init`, `build`, `run`,
  `test`, `add`, `clean`, `package`, `publish`, `jdk`, `tree`, `why`,
  `plugins`, `import`) and their flags will not change incompatibly in
  the `1.x` line.
- **Plugin protocol is stable.** Third-party plugins are `jet-<name>`
  binaries on `PATH`, invoked with the remaining argv plus
  `JET_PROJECT_ROOT` and `JET_VERSION` environment variables. This
  contract will hold for the entire `1.x` line.
- **Lockfile format is stable.** `jet.lock` `version = 1` will keep
  parsing; new fields will be optional.

### Added

- `[package].edition` field (defaults to `"2026"` for new projects via
  `jet new`).
- `jet import` — converts a Maven `pom.xml` to `jet.toml`. Extracts
  coords, Java version (from `maven.compiler.{release,target}` or
  `java.version`), and dependencies (compile/runtime → main, test → dev).
  Skips `provided`/`system`/BOM imports with explanatory warnings.

### Release cadence

- **Patch (1.0.x)**: as needed for bug fixes; aim for none-or-fast.
- **Minor (1.x.0)**: roughly every 6 weeks, additive only — new commands,
  flags, manifest fields, plugin hooks. Existing surfaces unchanged.
- **Major (2.0.0)**: only when an incompatible change is unavoidable.
  Pre-announced with at least one full minor cycle of deprecation.

### Deprecation policy

A feature deprecated in `1.x.0` keeps working through every subsequent
`1.x` release until the next major. Deprecation is announced in the
changelog, in the CLI via a `warning:` line, and in this document.

## [0.9.0] — 2026-05-11

- `jet tree`: box-drawing dependency tree (`--scope` filter).
- `jet why <coord>`: BFS paths from manifest roots to a coord.
- `jet plugins`: list `jet-*` binaries on PATH.
- External subcommand dispatch (git-style): `jet <name>` → `jet-<name>`
  with `JET_PROJECT_ROOT` and `JET_VERSION` env, forwarded argv.

## [0.8.0] — 2026-05-11

- `[toolchain]` table with `version` + `vendor` (default `temurin`).
- `jet jdk install <version>`: download JDK from Adoptium, extract to
  `~/.jet/jdks/<vendor>-<version>/`. macOS `.jdk` bundle layout handled.
- `jet jdk list`.
- `find_javac_for(manifest)` / `find_java_for(manifest)` route through
  the jet-managed JDK when `[toolchain]` is set.

## [0.7.0] — 2026-05-10

- Content-addressed build cache at `~/Library/Caches/jet/build/<hash>/`
  (or `$XDG_CACHE_HOME/jet/build/`). Cache hit skips `javac` entirely;
  measured **212ms → 1ms** on a single-source project (200× speedup).
- `--no-cache` flag.

## [0.6.0] — 2026-05-10

- `jet publish [--dry-run] [--no-sign]`: builds, generates POM, sources
  JAR, MD5 + SHA-1 checksums, optional GPG `.asc` signatures, and
  uploads to a Maven-layout HTTP endpoint (or stages under
  `target/publish/` for review).
- `[publish]` schema: url, homepage, repository, scm-connection,
  scm-developer-connection, sign, gpg-key, license-url.
- POM generator with SPDX → license URL mapping, parsed
  `Name <email>` developers, scm, dependencies from lockfile filtered
  to `origin = main` + `scope ∈ {compile, runtime}`.

## [0.5.4] — 2026-05-10

- Shared workspace-root `jet.lock`. `Workspace::ensure_lockfile` runs
  once per build, unioning every member's deps; per-member builds load
  from the shared lock. Legacy per-member locks detected and warned.

## [0.5.3] — 2026-05-10

- `[workspace.package]` field inheritance via TOML pre-processing. Members
  opt in per field with `field.workspace = true`. Inheritable fields:
  version, java, group, license, authors, description.

## [0.5.2] — 2026-05-08

- `[workspace.dependencies]` + `dep.workspace = true`. Workspace owns
  version; member layers scope/classifier/type/exclude/optional.

## [0.5.1] — 2026-05-08

- Glob patterns in `members` (`crates/*`).
- Shared workspace-root `target/` (`target/classes/<member>/` etc.).
- Parallel build scheduler with `--jobs N` flag (default
  `available_parallelism()`). `[member]` prefix on log lines.

## [0.5.0] — 2026-05-08

- Workspaces. `[workspace]` table with `members`, `exclude`,
  `default-members`. Path dependencies (`{ path = "../foo" }`).
- Topological build order via Kahn's algorithm with cycle detection.
- `jet build -p <member>` scopes to that member + path-dep ancestors.

## [0.4.0] — 2026-05-08

- `jet package [--uber]`. Reproducible JAR builder (sorted entries,
  fixed mtime, fixed permissions). MANIFEST.MF generation with 72-byte
  line wrapping. Uber mode merges dep JARs with service-file
  concatenation, signature stripping, and LICENSE/NOTICE renaming.
- Two consecutive `jet package --uber` builds produce byte-identical
  SHA-256.

## [0.3.0] — 2026-05-08

- `jet test [filter]`. JUnit 5 (Jupiter aggregate) auto-resolved from
  `[dev-dependencies]`; junit-platform-console-standalone fetched and
  cached. Surefire-compatible XML reports under `target/test-reports/`.
- Smart filter dispatch: `Foo::bar` → `--select-method`, `com.pkg.Foo`
  → `--select-class`, `com.pkg.*` → `--select-package`, lowercase →
  `--include-classname '.*x.*'`.
- `[dev-dependencies]` table; origin-tagged single-pass resolver
  (main wins on overlap); `jet add --dev`.
- `jet new` scaffolds a sample JUnit test.

## [0.2.0] — 2026-05-08

- Maven Central dependency resolution. POM parser (quick-xml streaming)
  with parent-chain, properties, BOM imports, exclusions. Transitive
  resolution with nearest-wins (Maven default). `jet.lock` (TOML, sorted,
  atomic write). `jet add <group:artifact:version>`. Version ranges
  rejected with a clear error.

## [0.1.0] — 2026-05-08

- Initial release. `jet new`, `jet init`, `jet build` (javac subprocess,
  sha256-fingerprint incremental cache), `jet run` (auto-detects main),
  `jet clean`. JDK detection chain: `JAVA_HOME` → `PATH` →
  `/usr/libexec/java_home` (macOS).
- Name validation: Java keywords, Windows reserved names, build-tool
  reserved names.
