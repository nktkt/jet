//! Workspace discovery, member loading, and the path-dep DAG.
//!
//! v0.5 MVP: literal member paths (no glob expansion), no field inheritance
//! (`workspace = true` syntax is deferred), Kahn's-algorithm topological order
//! over path dependencies. A future revision will add globs, parallel build
//! scheduling, and workspace-package / workspace-dependencies inheritance.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::manifest::Manifest;

/// One workspace member, fully loaded.
pub struct Member {
    /// Member directory (absolute, canonicalized when possible).
    pub path: PathBuf,
    pub manifest: Manifest,
    pub name: String,
}

pub struct Workspace {
    pub root: PathBuf,
    pub members: Vec<Member>,
    /// `name -> index in members`.
    pub by_name: HashMap<String, usize>,
}

impl Workspace {
    /// Discover the workspace by walking up from `start`. Returns:
    /// - the workspace if a `[workspace]` manifest is found, or
    /// - a single-member "implicit" workspace wrapping the nearest `jet.toml`.
    pub fn discover(start: &Path) -> Result<Self> {
        let root_manifest_dir = Manifest::find_root(start)?;
        let root_manifest = Manifest::load(&root_manifest_dir)?;

        if let Some(ws) = &root_manifest.workspace {
            let mut excluded: std::collections::HashSet<PathBuf> = Default::default();
            for e in &ws.exclude {
                let canon = canonicalize(&root_manifest_dir.join(e));
                excluded.insert(canon);
            }

            // Expand member patterns through `glob`. Patterns without globbing
            // metacharacters fall through as literal paths, matching cargo.
            let mut paths: Vec<PathBuf> = Vec::new();
            let mut seen: std::collections::HashSet<PathBuf> = Default::default();
            for raw in &ws.members {
                let pattern = root_manifest_dir.join(raw);
                let pattern_str = pattern.to_string_lossy();
                let is_glob = raw.contains('*') || raw.contains('?') || raw.contains('[');
                if is_glob {
                    let entries = glob::glob(&pattern_str)
                        .with_context(|| format!("invalid glob `{raw}`"))?;
                    for entry in entries.filter_map(|e| e.ok()) {
                        if entry.is_dir() && entry.join("jet.toml").is_file() {
                            let canon = canonicalize(&entry);
                            if !excluded.contains(&canon) && seen.insert(canon.clone()) {
                                paths.push(entry);
                            }
                        }
                    }
                } else {
                    let canon = canonicalize(&pattern);
                    if !excluded.contains(&canon) && seen.insert(canon.clone()) {
                        paths.push(pattern);
                    }
                }
            }
            // Stable order: lex by canonical path so builds are reproducible.
            paths.sort_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));

            let mut members: Vec<Member> = Vec::with_capacity(paths.len());
            for path in paths {
                let manifest = Manifest::load(&path).with_context(|| {
                    format!("loading workspace member at {}", path.display())
                })?;
                let name = manifest
                    .pkg()
                    .with_context(|| {
                        format!("member at `{}` has no [package] table", path.display())
                    })?
                    .name
                    .clone();
                members.push(Member { path, manifest, name });
            }
            // Reject duplicate member names.
            let mut seen: std::collections::HashSet<&str> = Default::default();
            for m in &members {
                if !seen.insert(&m.name) {
                    bail!("duplicate workspace member name `{}`", m.name);
                }
            }
            let by_name = members
                .iter()
                .enumerate()
                .map(|(i, m)| (m.name.clone(), i))
                .collect();
            Ok(Self { root: root_manifest_dir, members, by_name })
        } else {
            // Implicit single-member workspace.
            let pkg = root_manifest.pkg().with_context(|| {
                format!(
                    "manifest at `{}` has neither [package] nor [workspace]",
                    root_manifest_dir.display()
                )
            })?;
            let name = pkg.name.clone();
            let mut by_name = HashMap::new();
            by_name.insert(name.clone(), 0);
            Ok(Self {
                root: root_manifest_dir.clone(),
                members: vec![Member {
                    path: root_manifest_dir,
                    manifest: root_manifest,
                    name,
                }],
                by_name,
            })
        }
    }

    pub fn is_explicit_workspace(&self) -> bool {
        self.members.len() > 1
            || self
                .members
                .first()
                .is_some_and(|m| m.manifest.workspace.is_some())
    }

    /// Build the path-dep DAG and return members in topological build order
    /// (deps first).
    pub fn topological_order(&self) -> Result<Vec<usize>> {
        // adjacency[u] = set of members u depends on (its path-deps)
        let n = self.members.len();
        let mut adjacency: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut in_degree: Vec<usize> = vec![0; n];

        for (i, member) in self.members.iter().enumerate() {
            for (key, spec) in &member.manifest.dependencies {
                let Some(rel_path) = spec.path() else { continue };
                let target_dir = canonicalize(&member.path.join(rel_path));
                let dep_idx = self.members.iter().position(|m| {
                    canonicalize(&m.path) == target_dir
                });
                let dep_idx = match dep_idx {
                    Some(j) => j,
                    None => bail!(
                        "member `{}` depends on path `{}` (key `{key}`) \
                         which is not a workspace member",
                        member.name,
                        rel_path
                    ),
                };
                if dep_idx == i {
                    bail!("member `{}` cannot depend on itself", member.name);
                }
                adjacency[i].push(dep_idx);
                in_degree[i] += 1;
            }
        }

        // Kahn's algorithm.
        let mut queue: VecDeque<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
        let mut order: Vec<usize> = Vec::with_capacity(n);
        let mut remaining_in: Vec<usize> = in_degree.clone();
        // Reverse adjacency for decrementing in-degree on completion.
        let mut dependents_of: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (u, deps) in adjacency.iter().enumerate() {
            for &d in deps {
                dependents_of[d].push(u);
            }
        }

        while let Some(u) = queue.pop_front() {
            order.push(u);
            for &v in &dependents_of[u] {
                remaining_in[v] -= 1;
                if remaining_in[v] == 0 {
                    queue.push_back(v);
                }
            }
        }
        if order.len() != n {
            // Cycle: identify members still with in-degree > 0.
            let stuck: Vec<&str> = (0..n)
                .filter(|i| remaining_in[*i] > 0)
                .map(|i| self.members[i].name.as_str())
                .collect();
            bail!("path-dep cycle among members: [{}]", stuck.join(", "));
        }
        Ok(order)
    }

    /// Compute the closure of a member plus all its path-dep ancestors
    /// (transitive). Used by `-p`.
    pub fn closure(&self, root_idx: usize) -> Result<Vec<usize>> {
        let mut included: std::collections::HashSet<usize> = Default::default();
        let mut stack: Vec<usize> = vec![root_idx];
        while let Some(u) = stack.pop() {
            if !included.insert(u) {
                continue;
            }
            for (_, spec) in &self.members[u].manifest.dependencies {
                let Some(rel_path) = spec.path() else { continue };
                let target_dir = canonicalize(&self.members[u].path.join(rel_path));
                if let Some(j) = self
                    .members
                    .iter()
                    .position(|m| canonicalize(&m.path) == target_dir)
                {
                    stack.push(j);
                }
            }
        }
        // Filter the topological order to just the included members.
        let topo = self.topological_order()?;
        Ok(topo.into_iter().filter(|i| included.contains(i)).collect())
    }

    pub fn find_member(&self, name: &str) -> Result<usize> {
        self.by_name.get(name).copied().ok_or_else(|| {
            let names: Vec<&str> = self.members.iter().map(|m| m.name.as_str()).collect();
            anyhow::anyhow!(
                "no workspace member named `{name}` (members: [{}])",
                names.join(", ")
            )
        })
    }

    pub fn default_members(&self) -> Vec<usize> {
        let ws = match self.members.first().and_then(|m| m.manifest.workspace.as_ref()) {
            Some(ws) if !ws.default_members.is_empty() => ws,
            _ => return (0..self.members.len()).collect(),
        };
        ws.default_members
            .iter()
            .filter_map(|name| self.by_name.get(name).copied())
            .collect()
    }
}

fn canonicalize(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// Run a closure across workspace members in topological order, with up to
/// `jobs` of them executing concurrently. Members are kept in `order` and
/// the per-member dependency edges (member-idx → its path-dep ancestors)
/// determine readiness. The closure receives the member index and a list of
/// the indices of its already-finished path-dep ancestors so it can collect
/// their outputs without locking the shared map (callers pass in an
/// `Arc<Mutex<HashMap>>` of outputs they own).
///
/// Fails fast on the first error: stops dispatching new work and waits for
/// in-flight workers to drain before returning.
pub fn parallel_run<T, F>(
    workspace: &Workspace,
    order: &[usize],
    jobs: usize,
    work: F,
) -> Result<std::collections::HashMap<usize, T>>
where
    T: Send + 'static,
    F: Fn(usize) -> Result<T> + Send + Sync + 'static,
{
    use std::sync::{Arc, Mutex};
    use std::sync::atomic::{AtomicBool, Ordering};

    let n = order.len();
    if n == 0 {
        return Ok(Default::default());
    }

    // Per-member in-degree (count of path-dep ancestors that must finish first).
    let mut adjacency: Vec<Vec<usize>> = vec![Vec::new(); workspace.members.len()];
    let mut in_degree: Vec<usize> = vec![0; workspace.members.len()];
    let included: std::collections::HashSet<usize> = order.iter().copied().collect();
    for &i in order {
        for (_, spec) in &workspace.members[i].manifest.dependencies {
            let Some(rel) = spec.path() else { continue };
            let target = canonicalize(&workspace.members[i].path.join(rel));
            if let Some(j) = workspace.members.iter().position(|m| canonicalize(&m.path) == target) {
                if included.contains(&j) {
                    adjacency[j].push(i); // j -> i (j unlocks i)
                    in_degree[i] += 1;
                }
            }
        }
    }

    let work = Arc::new(work);
    let outputs: Arc<Mutex<std::collections::HashMap<usize, T>>> =
        Arc::new(Mutex::new(std::collections::HashMap::with_capacity(n)));
    let in_degree = Arc::new(Mutex::new(in_degree));
    let cancel = Arc::new(AtomicBool::new(false));
    let first_error: Arc<Mutex<Option<anyhow::Error>>> = Arc::new(Mutex::new(None));

    // Channels: ready queue (coordinator → workers), completion (workers → coordinator).
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<usize>();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<(usize, Result<T>)>();
    let ready_rx = Arc::new(Mutex::new(ready_rx));

    // Seed initially-ready members.
    let mut remaining = n;
    {
        let deg = in_degree.lock().unwrap();
        for &i in order {
            if deg[i] == 0 {
                ready_tx.send(i).expect("ready queue open");
            }
        }
    }

    let n_workers = jobs.max(1);
    let mut handles = Vec::with_capacity(n_workers);
    for _ in 0..n_workers {
        let rx = Arc::clone(&ready_rx);
        let tx = done_tx.clone();
        let work = Arc::clone(&work);
        let cancel = Arc::clone(&cancel);
        handles.push(std::thread::spawn(move || {
            loop {
                let next = {
                    let rx = rx.lock().unwrap();
                    rx.recv()
                };
                let idx = match next {
                    Ok(i) => i,
                    Err(_) => break,
                };
                if cancel.load(Ordering::Relaxed) {
                    // Drain quietly without running.
                    continue;
                }
                let result = work(idx);
                if tx.send((idx, result)).is_err() {
                    break;
                }
            }
        }));
    }
    drop(done_tx);

    while remaining > 0 {
        let (idx, result) = match done_rx.recv() {
            Ok(v) => v,
            Err(_) => break,
        };
        remaining -= 1;
        match result {
            Ok(out) => {
                outputs.lock().unwrap().insert(idx, out);
                // Decrement in-degrees of dependents.
                let mut deg = in_degree.lock().unwrap();
                for &dep in &adjacency[idx] {
                    deg[dep] = deg[dep].saturating_sub(1);
                    if deg[dep] == 0 && !cancel.load(Ordering::Relaxed) {
                        let _ = ready_tx.send(dep);
                    }
                }
            }
            Err(e) => {
                cancel.store(true, Ordering::Relaxed);
                let mut slot = first_error.lock().unwrap();
                if slot.is_none() {
                    *slot = Some(e);
                }
            }
        }
    }
    drop(ready_tx);

    for h in handles {
        let _ = h.join();
    }

    if let Some(e) = first_error.lock().unwrap().take() {
        return Err(e);
    }
    let map = Arc::try_unwrap(outputs)
        .map_err(|_| anyhow::anyhow!("internal: outputs Arc not unique"))?
        .into_inner()
        .map_err(|_| anyhow::anyhow!("internal: outputs Mutex poisoned"))?;
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, contents: &str) {
        fs::create_dir_all(dir.parent().unwrap()).unwrap();
        fs::write(dir, contents).unwrap();
    }

    #[test]
    fn implicit_single_member_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("jet.toml"),
            "[package]\nname = \"solo\"\nversion = \"0.1.0\"\njava = 21\n",
        );
        let ws = Workspace::discover(tmp.path()).unwrap();
        assert_eq!(ws.members.len(), 1);
        assert_eq!(ws.members[0].name, "solo");
        assert!(!ws.is_explicit_workspace());
    }

    #[test]
    fn explicit_workspace_topo_order() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("jet.toml"),
            "[workspace]\nmembers = [\"core\", \"api\", \"cli\"]\n",
        );
        write(
            &tmp.path().join("core/jet.toml"),
            "[package]\nname = \"core\"\nversion = \"0.1.0\"\njava = 21\n",
        );
        write(
            &tmp.path().join("api/jet.toml"),
            "[package]\nname = \"api\"\nversion = \"0.1.0\"\njava = 21\n\n\
             [dependencies]\n\"local:core\" = { path = \"../core\" }\n",
        );
        write(
            &tmp.path().join("cli/jet.toml"),
            "[package]\nname = \"cli\"\nversion = \"0.1.0\"\njava = 21\n\n\
             [dependencies]\n\"local:api\" = { path = \"../api\" }\n",
        );
        let ws = Workspace::discover(tmp.path()).unwrap();
        assert_eq!(ws.members.len(), 3);
        let order: Vec<&str> = ws
            .topological_order()
            .unwrap()
            .into_iter()
            .map(|i| ws.members[i].name.as_str())
            .collect();
        // core must come before api, api before cli.
        let pos = |n: &str| order.iter().position(|&s| s == n).unwrap();
        assert!(pos("core") < pos("api"));
        assert!(pos("api") < pos("cli"));
    }

    #[test]
    fn detects_path_dep_cycle() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("jet.toml"),
            "[workspace]\nmembers = [\"a\", \"b\"]\n",
        );
        write(
            &tmp.path().join("a/jet.toml"),
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\njava = 21\n\n\
             [dependencies]\n\"x:b\" = { path = \"../b\" }\n",
        );
        write(
            &tmp.path().join("b/jet.toml"),
            "[package]\nname = \"b\"\nversion = \"0.1.0\"\njava = 21\n\n\
             [dependencies]\n\"x:a\" = { path = \"../a\" }\n",
        );
        let ws = Workspace::discover(tmp.path()).unwrap();
        let err = ws.topological_order().expect_err("should detect cycle");
        let msg = format!("{err:#}");
        assert!(msg.contains("cycle"), "expected cycle error, got: {msg}");
    }

    #[test]
    fn closure_includes_only_path_ancestors() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("jet.toml"),
            "[workspace]\nmembers = [\"core\", \"api\", \"cli\", \"unrelated\"]\n",
        );
        write(
            &tmp.path().join("core/jet.toml"),
            "[package]\nname = \"core\"\nversion = \"0.1.0\"\njava = 21\n",
        );
        write(
            &tmp.path().join("api/jet.toml"),
            "[package]\nname = \"api\"\nversion = \"0.1.0\"\njava = 21\n\n\
             [dependencies]\n\"x:core\" = { path = \"../core\" }\n",
        );
        write(
            &tmp.path().join("cli/jet.toml"),
            "[package]\nname = \"cli\"\nversion = \"0.1.0\"\njava = 21\n\n\
             [dependencies]\n\"x:api\" = { path = \"../api\" }\n",
        );
        write(
            &tmp.path().join("unrelated/jet.toml"),
            "[package]\nname = \"unrelated\"\nversion = \"0.1.0\"\njava = 21\n",
        );
        let ws = Workspace::discover(tmp.path()).unwrap();
        let cli_idx = ws.find_member("cli").unwrap();
        let names: Vec<&str> = ws
            .closure(cli_idx)
            .unwrap()
            .into_iter()
            .map(|i| ws.members[i].name.as_str())
            .collect();
        assert!(names.contains(&"core"));
        assert!(names.contains(&"api"));
        assert!(names.contains(&"cli"));
        assert!(!names.contains(&"unrelated"));
    }
}
