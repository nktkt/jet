# jet

> A fast, modern build tool for the JVM. Inspired by [Cargo](https://doc.rust-lang.org/cargo/) and [Bun](https://bun.sh).

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Status: 1.0](https://img.shields.io/badge/status-1.0-brightgreen.svg)](CHANGELOG.md)

`jet` is an experiment in answering one question: **what would a Java build tool look like if it were designed in 2026 instead of inheriting decisions from 2004?**

Maven gives you XML and ceremony. Gradle gives you a Turing-complete DSL and a daemon. `jet` gives you a single TOML file, a fast static binary, and a CLI that gets out of your way.

```toml
# jet.toml
[package]
name = "hello"
version = "0.1.0"
java  = "21"

[dependencies]
"org.slf4j:slf4j-api"        = "2.0.13"
"com.fasterxml.jackson.core:jackson-databind" = "2.17.2"

[dev-dependencies]
"org.junit.jupiter:junit-jupiter" = "5.10.2"
```

```bash
jet new my-app          # scaffold a new project
jet add com.google.guava:guava:33.2.0-jre
jet build               # incremental compile
jet run -- --port 8080  # run main class
jet test                # JUnit 5 by default
jet package             # produce a jar (or uber-jar)
```

---

## Status

**1.0 — stable.** The manifest schema, CLI surface, plugin protocol, and
lockfile format are frozen for the `1.x` line. See [CHANGELOG.md](./CHANGELOG.md)
for the release history and the [Roadmap](./ROADMAP.md) for what's
already in the box (everything 0.1 → 1.0). Post-1.0 work (registries,
remote build cache, native image, watch mode) is tracked under "Beyond 1.0".

## Why another build tool?

| Pain point in Maven / Gradle | What `jet` aims to do |
|---|---|
| XML / Groovy / Kotlin DSL boilerplate | One short, declarative `jet.toml` |
| Slow cold start (Gradle daemon, JVM) | Native binary, sub-second startup |
| Opaque dependency resolution | `jet why <coord>` shows resolution paths |
| Plugin ecosystem fragmentation | Small, sharp built-ins; thin plugin API later |
| Reproducibility headaches | Lockfile (`jet.lock`) on by default |
| Multi-module ergonomics | Workspaces, like Cargo |

## Goals

- **Fast.** Native binary, parallel by default, content-addressed build cache, sub-second startup.
- **Simple.** One config file. Convention over configuration. No build script that is itself a program.
- **Compatible.** Resolve from Maven Central and any Maven-format repository on day one. Produce standard JARs, POMs, and Gradle Module Metadata.
- **Reproducible.** Locked dependency versions, hermetic toolchain (`jet` downloads the JDK if you ask).
- **Honest.** No magic. `jet --verbose` shows you every javac invocation and every cache hit.

## Non-goals (for now)

- Replacing Gradle for Android. Android has its own deep integrations; `jet` targets server, library, and CLI projects first.
- A scripting DSL. If you need full programming power in your build, this is not the tool. Use a `build.rs`-style hook for the rare escape hatch.
- Polyglot builds. `jet` is for the JVM. Bazel exists if you need cross-language.

## Quickstart (planned UX)

```bash
# install (planned)
curl -fsSL https://jet.build/install.sh | sh

# create + run
jet new hello && cd hello
jet run
# > Hello from jet!
```

## Project layout

```
my-app/
├── jet.toml          # project manifest
├── jet.lock          # resolved dependencies (committed)
├── src/
│   ├── main/java/    # production sources
│   └── test/java/    # test sources
└── target/           # build outputs (gitignored)
```

## Roadmap

See [ROADMAP.md](./ROADMAP.md). Short version:

- **0.1** — `new`, `build`, `run` against a single source tree
- **0.2** — Maven Central resolution + lockfile
- **0.3** — JUnit 5 test runner
- **0.4** — Packaging (thin + uber jar)
- **0.5** — Workspaces (multi-module)
- **0.6** — Publishing to Maven repositories
- **1.0** — Stable manifest format, plugin API frozen

## Building from source

```bash
git clone https://github.com/nktkt/jet
cd jet
cargo build --release
./target/release/jet --help
```

Requires Rust 1.85+ (edition 2024) and a JDK on `PATH` for end-to-end testing.

## Plugin protocol (1.0)

`jet` plugins are external executables on `PATH`, named `jet-<name>`. When
`jet <name>` doesn't match a built-in command, jet forwards to
`jet-<name>` with the remaining `argv` and the following environment:

| Variable           | Set when                              | Value                          |
|--------------------|---------------------------------------|--------------------------------|
| `JET_PROJECT_ROOT` | a `jet.toml` is found by walking up   | absolute path to project root  |
| `JET_VERSION`      | always                                | jet's `CARGO_PKG_VERSION`      |

This contract is frozen for the `1.x` line. A plugin is a single
executable in any language — Bash, Python, Go, anything that can read
`JET_PROJECT_ROOT` and emit text.

`jet plugins` lists every `jet-*` binary visible on `PATH`.

## Contributing

Contributions are very welcome — `jet` is in the design phase, so opinions are worth more than code right now. Please open an issue before sending a large PR so we can align on direction.

## Prior art and inspiration

- **Cargo** — manifest format, lockfile, workspace model, command UX
- **Bun** — speed-as-a-feature, single-binary install, replacing slow incumbents
- **Mill** — proving that fast, simpler JVM builds are possible
- **Bazel** — content-addressed caching ideas

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](./LICENSE-APACHE))
- MIT license ([LICENSE-MIT](./LICENSE-MIT))

at your option.
