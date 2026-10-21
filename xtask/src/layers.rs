//! `cargo xtask layers`: each crate depends only on strictly lower layers, and nothing below the UI layer
//! (L6) pulls in a GUI toolkit, directly or transitively. Test-support crates (local test servers) may be
//! used only as dev-dependencies, or by apps and tooling.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use crate::TaskResult;
use crate::metadata::{self, Metadata};

/// Package → layer. Apps sit on L7, repository tooling on L8, test support on [`TEST_LAYER`].
pub const LAYERS: &[(&str, u8)] = &[
    ("mailcraft-geom", 0),
    ("mailcraft-mime", 0),
    ("mailcraft-model", 1),
    ("mailcraft-fonts", 1),
    ("mailcraft-store", 2),
    ("mailcraft-html", 2),
    ("mailcraft-layout", 2),
    ("mailcraft-render", 3),
    ("mailcraft-net", 3),
    ("mailcraft-formats", 3),
    ("mailcraft-search", 3),
    ("mailcraft-proof", 3),
    ("mailcraft-smime", 3),
    ("mailcraft-engine", 4),
    ("mailcraft-mcp", 5),
    ("mailcraft-ui-egui", 6),
    ("mailcraft", 7),
    ("mailcraft-cli", 7),
    ("mailcraft-web", 7),
    ("mailcraft-testmail-app", 7),
    ("xtask", 8),
    ("mailcraft-testmail", TEST_LAYER),
];

/// Test-support layer: crates on it may depend on L0–L6, and only dev-dependencies, apps (L7) and tooling
/// (L8) may depend on them. Shipped library code must never link a test server.
pub const TEST_LAYER: u8 = 9;

/// The apps layer: the lowest layer allowed a normal dependency on a test-support crate.
pub const APPS_LAYER: u8 = 7;

/// Same-layer dependencies that are allowed: (from, to). None so far.
pub const SAME_LAYER_EXCEPTIONS: &[(&str, &str)] = &[];

/// The UI layer: crates below it must not reach these packages.
pub const UI_LAYER: u8 = 6;
pub const GUI_CRATES: &[&str] = &["egui", "eframe", "winit", "wgpu", "glow", "rfd"];

pub fn layer_of(name: &str) -> Option<u8> {
    LAYERS.iter().find(|(n, _)| *n == name).map(|(_, l)| *l)
}

fn layer_name(l: u8) -> String {
    match l {
        7 => "apps".to_owned(),
        8 => "tools".to_owned(),
        TEST_LAYER => "test".to_owned(),
        n => format!("L{n}"),
    }
}

/// All problems found in `meta` (empty = OK).
pub fn check(meta: &Metadata) -> Vec<String> {
    let mut problems = Vec::new();
    let members = meta.members();
    let member_names: BTreeSet<&str> = members.iter().map(|p| p.name.as_str()).collect();
    for p in &members {
        let Some(layer) = layer_of(&p.name) else {
            problems.push(format!(
                "crate `{}` has no layer: add it to LAYERS in xtask/src/layers.rs (and plan/architecture.md)",
                p.name
            ));
            continue;
        };
        for (dep, kind) in &p.dependencies {
            if kind.as_deref() == Some("dev") || !member_names.contains(dep.as_str()) {
                continue;
            }
            let Some(dep_layer) = layer_of(dep) else { continue };
            let allowed = if dep_layer == TEST_LAYER {
                (APPS_LAYER..TEST_LAYER).contains(&layer)
            } else if layer == TEST_LAYER {
                dep_layer < APPS_LAYER
            } else {
                dep_layer < layer
                    || (dep_layer == layer
                        && SAME_LAYER_EXCEPTIONS.contains(&(p.name.as_str(), dep.as_str())))
            };
            if !allowed && dep_layer == TEST_LAYER {
                problems.push(format!(
                    "`{}` ({}) depends on the test-support crate `{dep}`: use it as a dev-dependency only",
                    p.name,
                    layer_name(layer)
                ));
            } else if !allowed {
                problems.push(format!(
                    "`{}` ({}) depends on `{dep}` ({}): a crate may depend only on lower layers",
                    p.name,
                    layer_name(layer),
                    layer_name(dep_layer)
                ));
            }
        }
    }
    problems.extend(gui_leaks(meta));
    problems.extend(desktop_links(meta));
    problems
}

/// The desktop app must be able to open links: egui-winit only does that with its `links` feature, which
/// brings in `webbrowser`. Without it every `open_url` (message links, OAuth sign-in pages) is dropped.
fn desktop_links(meta: &Metadata) -> Vec<String> {
    let Some(resolve) = &meta.resolve else { return Vec::new() };
    let Some(app) = meta.members().into_iter().find(|p| p.name == "mailcraft") else { return Vec::new() };
    let graph: BTreeMap<&str, Vec<&str>> = resolve
        .iter()
        .map(|(id, deps)| {
            (id.as_str(), deps.iter().filter(|d| d.non_dev()).map(|d| d.pkg.as_str()).collect())
        })
        .collect();
    let mut seen: BTreeSet<&str> = BTreeSet::from([app.id.as_str()]);
    let mut queue = VecDeque::from([app.id.as_str()]);
    while let Some(id) = queue.pop_front() {
        if meta.package(id).is_some_and(|p| p.name == "webbrowser") {
            return Vec::new();
        }
        for &dep in graph.get(id).map(Vec::as_slice).unwrap_or(&[]) {
            if seen.insert(dep) {
                queue.push_back(dep);
            }
        }
    }
    vec![
        "`mailcraft` does not depend on `webbrowser`: links and OAuth sign-in pages cannot open \
         (enable eframe's `links` feature)"
            .to_owned(),
    ]
}

/// Crates below L6 that reach a GUI crate through normal/build edges, with the path.
fn gui_leaks(meta: &Metadata) -> Vec<String> {
    let Some(resolve) = &meta.resolve else { return Vec::new() };
    let graph: BTreeMap<&str, Vec<&str>> = resolve
        .iter()
        .map(|(id, deps)| {
            (id.as_str(), deps.iter().filter(|d| d.non_dev()).map(|d| d.pkg.as_str()).collect())
        })
        .collect();
    let name = |id: &str| meta.package(id).map(|p| p.name.clone()).unwrap_or_else(|| id.to_owned());
    let mut out = Vec::new();
    for p in meta.members() {
        if layer_of(&p.name).is_none_or(|l| l >= UI_LAYER && l != TEST_LAYER) {
            continue;
        }
        // Breadth-first search keeps the shortest path to each package.
        let mut prev: BTreeMap<&str, &str> = BTreeMap::new();
        let mut seen: BTreeSet<&str> = BTreeSet::from([p.id.as_str()]);
        let mut queue = VecDeque::from([p.id.as_str()]);
        while let Some(id) = queue.pop_front() {
            for &dep in graph.get(id).map(Vec::as_slice).unwrap_or(&[]) {
                if !seen.insert(dep) {
                    continue;
                }
                prev.insert(dep, id);
                if GUI_CRATES.contains(&name(dep).as_str()) {
                    let mut chain = vec![name(dep)];
                    let mut cur = dep;
                    while let Some(&before) = prev.get(cur) {
                        chain.push(name(before));
                        cur = before;
                    }
                    chain.reverse();
                    out.push(format!(
                        "`{}` ({}) pulls in the GUI crate `{}`: {}",
                        p.name,
                        layer_name(layer_of(&p.name).unwrap_or(0)),
                        name(dep),
                        chain.join(" → ")
                    ));
                } else {
                    queue.push_back(dep);
                }
            }
        }
    }
    out
}

pub fn run(root: &Path) -> TaskResult {
    let meta = metadata::load(root, true)?;
    let mut members = meta.members();
    members.sort_by_key(|p| (layer_of(&p.name).unwrap_or(u8::MAX), p.name.clone()));
    let member_names: BTreeSet<&str> = members.iter().map(|p| p.name.as_str()).collect();
    for p in &members {
        let deps: Vec<&str> = p
            .dependencies
            .iter()
            .filter(|(d, k)| k.as_deref() != Some("dev") && member_names.contains(d.as_str()))
            .map(|(d, _)| d.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let layer = layer_of(&p.name).map(layer_name).unwrap_or_else(|| "??".to_owned());
        println!("{layer:>5}  {:<24} {}", p.name, deps.join(", "));
    }
    let problems = check(&meta);
    if problems.is_empty() {
        println!("layers: OK ({} crates)", members.len());
        Ok(())
    } else {
        for p in &problems {
            println!("  ✗ {p}");
        }
        Err(format!("layers: {} problem(s)", problems.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{NodeDep, Package};

    fn pkg(name: &str, deps: &[&str]) -> Package {
        Package {
            id: format!("id-{name}"),
            name: name.to_owned(),
            dependencies: deps.iter().map(|d| ((*d).to_owned(), None)).collect(),
            ..Package::default()
        }
    }

    fn meta(pkgs: Vec<Package>, members: &[&str], edges: &[(&str, &str, Option<&str>)]) -> Metadata {
        let mut resolve: BTreeMap<String, Vec<NodeDep>> =
            pkgs.iter().map(|p| (p.id.clone(), Vec::new())).collect();
        for (from, to, kind) in edges {
            resolve
                .entry(format!("id-{from}"))
                .or_default()
                .push(NodeDep { pkg: format!("id-{to}"), kinds: vec![kind.map(str::to_owned)] });
        }
        Metadata {
            packages: pkgs,
            workspace_members: members.iter().map(|m| format!("id-{m}")).collect(),
            resolve: Some(resolve.into_iter().collect()),
        }
    }

    #[test]
    fn desktop_app_must_reach_webbrowser() {
        let without = meta(
            vec![pkg("mailcraft", &[]), pkg("egui-winit", &[])],
            &["mailcraft"],
            &[("mailcraft", "egui-winit", None)],
        );
        assert!(desktop_links(&without).iter().any(|p| p.contains("webbrowser")));
        let with = meta(
            vec![pkg("mailcraft", &[]), pkg("egui-winit", &[]), pkg("webbrowser", &[])],
            &["mailcraft"],
            &[("mailcraft", "egui-winit", None), ("egui-winit", "webbrowser", None)],
        );
        assert!(desktop_links(&with).is_empty());
        // Only as a dev-dependency does not count.
        let dev_only = meta(
            vec![pkg("mailcraft", &[]), pkg("webbrowser", &[])],
            &["mailcraft"],
            &[("mailcraft", "webbrowser", Some("dev"))],
        );
        assert!(!desktop_links(&dev_only).is_empty());
    }

    #[test]
    fn good_graph_passes() {
        let m = meta(
            vec![pkg("mailcraft-geom", &[]), pkg("mailcraft-model", &["mailcraft-geom"]), pkg("serde", &[])],
            &["mailcraft-geom", "mailcraft-model"],
            &[("mailcraft-model", "mailcraft-geom", None), ("mailcraft-model", "serde", None)],
        );
        assert!(check(&m).is_empty(), "{:?}", check(&m));
    }

    #[test]
    fn upward_same_layer_and_unknown_fail() {
        let m = meta(
            vec![
                pkg("mailcraft-geom", &["mailcraft-model"]),
                pkg("mailcraft-model", &["mailcraft-fonts"]),
                pkg("mailcraft-fonts", &[]),
                pkg("mailcraft-new", &[]),
            ],
            &["mailcraft-geom", "mailcraft-model", "mailcraft-fonts", "mailcraft-new"],
            &[],
        );
        let p = check(&m);
        assert_eq!(p.len(), 3, "{p:?}");
        assert!(p.iter().any(|s| s.contains("`mailcraft-geom` (L0) depends on `mailcraft-model` (L1)")));
        assert!(p.iter().any(|s| s.contains("`mailcraft-model` (L1) depends on `mailcraft-fonts`")));
        assert!(p.iter().any(|s| s.contains("`mailcraft-new` has no layer")));
    }

    #[test]
    fn test_support_only_as_dev_dependency_or_from_apps() {
        let m = meta(
            vec![
                pkg("mailcraft-testmail", &["mailcraft-model"]),
                pkg("mailcraft-model", &[]),
                pkg("mailcraft-testmail-app", &["mailcraft-testmail"]),
                pkg("mailcraft-net", &["mailcraft-testmail"]),
                pkg("mailcraft-engine", &[]),
            ],
            &[
                "mailcraft-testmail",
                "mailcraft-model",
                "mailcraft-testmail-app",
                "mailcraft-net",
                "mailcraft-engine",
            ],
            &[],
        );
        let p = check(&m);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].contains("`mailcraft-net` (L3) depends on the test-support crate"), "{p:?}");

        let mut dev = pkg("mailcraft-net", &[]);
        dev.dependencies = vec![("mailcraft-testmail".to_owned(), Some("dev".to_owned()))];
        let upward = pkg("mailcraft-testmail", &["mailcraft-engine", "mailcraft-cli"]);
        let m = meta(
            vec![dev, upward, pkg("mailcraft-engine", &[]), pkg("mailcraft-cli", &[])],
            &["mailcraft-net", "mailcraft-testmail", "mailcraft-engine", "mailcraft-cli"],
            &[],
        );
        let p = check(&m);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].contains("`mailcraft-testmail` (test) depends on `mailcraft-cli` (apps)"), "{p:?}");
    }

    #[test]
    fn transitive_gui_dependency_fails_but_dev_and_ui_are_fine() {
        let m = meta(
            vec![
                pkg("mailcraft-engine", &[]),
                pkg("mailcraft-ui-egui", &[]),
                pkg("helper", &[]),
                pkg("winit", &[]),
                pkg("egui", &[]),
            ],
            &["mailcraft-engine", "mailcraft-ui-egui"],
            &[
                ("mailcraft-engine", "helper", None),
                ("helper", "winit", Some("build")),
                ("mailcraft-engine", "egui", Some("dev")),
                ("mailcraft-ui-egui", "egui", None),
            ],
        );
        let p = check(&m);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].contains("mailcraft-engine → helper → winit"), "{p:?}");
    }
}
