//! `cargo metadata --format-version 1`, parsed with serde_json.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

#[derive(Clone, Debug, Default)]
pub struct Package {
    pub id: String,
    pub name: String,
    pub version: String,
    pub license: Option<String>,
    pub license_file: Option<String>,
    pub manifest_path: PathBuf,
    pub keywords: Vec<String>,
    pub categories: Vec<String>,
    /// Declared dependencies: (package name, kind: None = normal, "dev", "build").
    pub dependencies: Vec<(String, Option<String>)>,
}

#[derive(Clone, Debug, Default)]
pub struct NodeDep {
    pub pkg: String,
    /// Every kind this edge has (None = normal).
    pub kinds: Vec<Option<String>>,
}

impl NodeDep {
    /// Whether the edge is used outside tests (normal or build).
    pub fn non_dev(&self) -> bool {
        self.kinds.is_empty() || self.kinds.iter().any(|k| k.as_deref() != Some("dev"))
    }
}

#[derive(Clone, Debug, Default)]
pub struct Metadata {
    pub packages: Vec<Package>,
    pub workspace_members: BTreeSet<String>,
    /// Resolved graph (`None` with `--no-deps`): (package id, deps).
    pub resolve: Option<Vec<(String, Vec<NodeDep>)>>,
}

impl Metadata {
    pub fn package(&self, id: &str) -> Option<&Package> {
        self.packages.iter().find(|p| p.id == id)
    }
    pub fn members(&self) -> Vec<&Package> {
        self.packages.iter().filter(|p| self.workspace_members.contains(&p.id)).collect()
    }
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

fn opt_string(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(str::to_owned)
}

/// Parses `cargo metadata` JSON output.
pub fn parse(json: &Value) -> Result<Metadata, String> {
    let packages = json
        .get("packages")
        .and_then(Value::as_array)
        .ok_or("cargo metadata output has no `packages`")?
        .iter()
        .map(|p| Package {
            id: opt_string(p.get("id")).unwrap_or_default(),
            name: opt_string(p.get("name")).unwrap_or_default(),
            version: opt_string(p.get("version")).unwrap_or_default(),
            license: opt_string(p.get("license")),
            license_file: opt_string(p.get("license_file")),
            manifest_path: PathBuf::from(opt_string(p.get("manifest_path")).unwrap_or_default()),
            keywords: strings(p.get("keywords")),
            categories: strings(p.get("categories")),
            dependencies: p
                .get("dependencies")
                .and_then(Value::as_array)
                .map(|deps| {
                    deps.iter()
                        .filter_map(|d| Some((opt_string(d.get("name"))?, opt_string(d.get("kind")))))
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect();
    let workspace_members = strings(json.get("workspace_members")).into_iter().collect();
    let resolve = json.get("resolve").and_then(|r| r.get("nodes")).and_then(Value::as_array).map(|nodes| {
        nodes
            .iter()
            .map(|n| {
                let deps = n
                    .get("deps")
                    .and_then(Value::as_array)
                    .map(|deps| {
                        deps.iter()
                            .map(|d| NodeDep {
                                pkg: opt_string(d.get("pkg")).unwrap_or_default(),
                                kinds: d
                                    .get("dep_kinds")
                                    .and_then(Value::as_array)
                                    .map(|ks| ks.iter().map(|k| opt_string(k.get("kind"))).collect())
                                    .unwrap_or_default(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                (opt_string(n.get("id")).unwrap_or_default(), deps)
            })
            .collect()
    });
    Ok(Metadata { packages, workspace_members, resolve })
}

/// Runs `cargo metadata` in `root` (`--locked`: never rewrites Cargo.lock).
pub fn load(root: &Path, with_deps: bool) -> Result<Metadata, String> {
    let mut cmd = Command::new(crate::cargo());
    cmd.current_dir(root).args(["metadata", "--format-version", "1", "--locked"]);
    if !with_deps {
        cmd.arg("--no-deps");
    }
    let out = cmd.output().map_err(|e| format!("cannot run cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!("cargo metadata failed:\n{}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let json: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("cargo metadata printed invalid JSON: {e}"))?;
    parse(&json)
}
