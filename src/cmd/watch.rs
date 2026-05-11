//! `jet watch [build|test]` — re-run the build or test command whenever
//! source files change.
//!
//! Uses `notify` to recursively watch the project root, debounces events
//! (200 ms) to coalesce editor-multi-save bursts, and re-invokes the chosen
//! command. The v0.7 content-addressed build cache makes warm rebuilds
//! sub-second; the loop runs until Ctrl-C.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use notify::{
    Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher, event::ModifyKind,
};

use crate::manifest::Manifest;

const DEBOUNCE: Duration = Duration::from_millis(200);

#[derive(Clone, Copy)]
pub enum WatchAction {
    Build,
    Test,
}

pub struct WatchArgs {
    pub action: WatchAction,
}

pub fn cmd_watch(args: WatchArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let root = Manifest::find_root(&cwd)?;
    let (tx, rx) = channel::<notify::Result<Event>>();
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;
    watcher.configure(Config::default().with_poll_interval(Duration::from_secs(2)))?;

    let watch_paths = collect_watch_paths(&root);
    for p in &watch_paths {
        watcher
            .watch(p, RecursiveMode::Recursive)
            .with_context(|| format!("watching {}", p.display()))?;
    }
    let watch_paths_display = watch_paths
        .iter()
        .map(|p| p.strip_prefix(&root).unwrap_or(p).display().to_string())
        .collect::<Vec<_>>()
        .join(", ");

    eprintln!("[watch] watching {watch_paths_display} (Ctrl-C to stop)");

    // Initial run.
    run_action(args.action);

    // Event loop. After any relevant event arrives, drain the channel for
    // DEBOUNCE ms to coalesce, then run.
    loop {
        let first = match rx.recv() {
            Ok(Ok(ev)) => ev,
            Ok(Err(e)) => {
                eprintln!("[watch] notify error: {e:#}; continuing");
                continue;
            }
            Err(_) => break, // watcher dropped
        };
        if !is_relevant(&first, &root) {
            continue;
        }
        // Drain follow-up events within the debounce window.
        let deadline = Instant::now() + DEBOUNCE;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(remaining) {
                Ok(Ok(_ev)) => {} // swallow
                Ok(Err(_)) => {}  // swallow notify errors mid-batch
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
        }
        eprintln!("[watch] changes detected — rerunning");
        run_action(args.action);
    }
    Ok(())
}

/// Pick the directories to watch. Falls back to the project root if a
/// conventional subdir doesn't exist (covers projects in flux).
fn collect_watch_paths(root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for candidate in [
        "src/main/java",
        "src/main/resources",
        "src/test/java",
        "src/test/resources",
    ] {
        let p = root.join(candidate);
        if p.is_dir() {
            paths.push(p);
        }
    }
    let manifest = root.join("jet.toml");
    if manifest.is_file() {
        paths.push(manifest);
    }
    if paths.is_empty() {
        paths.push(root.to_path_buf());
    }
    paths
}

/// Skip events under build output and VCS dirs, plus pure access/metadata
/// notifications that don't represent a content change.
fn is_relevant(event: &Event, root: &Path) -> bool {
    let ignored_prefixes = [
        root.join("target"),
        root.join(".git"),
        root.join(".jet"),
        root.join("node_modules"),
    ];
    for path in &event.paths {
        for ign in &ignored_prefixes {
            if path.starts_with(ign) {
                return false;
            }
        }
    }
    matches!(
        event.kind,
        EventKind::Create(_)
            | EventKind::Remove(_)
            | EventKind::Modify(ModifyKind::Data(_))
            | EventKind::Modify(ModifyKind::Any)
            | EventKind::Modify(ModifyKind::Name(_))
    )
}

fn run_action(action: WatchAction) {
    let started = Instant::now();
    let result = match action {
        WatchAction::Build => {
            use super::build::{BuildArgs, cmd_build};
            cmd_build(BuildArgs {
                release: false,
                force_resolve: false,
                package: None,
                jobs: None,
                no_cache: false,
            })
        }
        WatchAction::Test => {
            use super::test::{TestArgs, cmd_test};
            cmd_test(TestArgs { filter: None })
        }
    };
    let elapsed = started.elapsed();
    match result {
        Ok(()) => eprintln!("[watch] done in {:.0}ms", elapsed.as_secs_f64() * 1000.0),
        Err(e) => eprintln!("[watch] failed in {:.0}ms: {e:#}", elapsed.as_secs_f64() * 1000.0),
    }
    eprintln!("[watch] waiting for changes…");
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, EventAttributes};

    fn event_with(path: &Path, kind: EventKind) -> Event {
        Event {
            kind,
            paths: vec![path.to_path_buf()],
            attrs: EventAttributes::default(),
        }
    }

    #[test]
    fn ignores_target_directory() {
        let root = Path::new("/proj");
        let ev = event_with(
            &root.join("target/classes/Foo.class"),
            EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Content)),
        );
        assert!(!is_relevant(&ev, root));
    }

    #[test]
    fn ignores_git_metadata() {
        let root = Path::new("/proj");
        let ev = event_with(
            &root.join(".git/HEAD"),
            EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Content)),
        );
        assert!(!is_relevant(&ev, root));
    }

    #[test]
    fn picks_up_source_changes() {
        let root = Path::new("/proj");
        let ev = event_with(
            &root.join("src/main/java/Foo.java"),
            EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Content)),
        );
        assert!(is_relevant(&ev, root));
    }

    #[test]
    fn picks_up_creates_and_removes() {
        let root = Path::new("/proj");
        for kind in [
            EventKind::Create(CreateKind::File),
            EventKind::Remove(notify::event::RemoveKind::File),
        ] {
            let ev = event_with(&root.join("src/main/java/Bar.java"), kind);
            assert!(is_relevant(&ev, root));
        }
    }
}
