//! `cargo xtask layers`: a crate may use workspace crates of lower layers only, and nothing below L6 may use a UI
//! toolkit (AGENTS.md "This repository").

use serde_json::Value;

/// Layer per package, without the `pr-craft-` prefix. `None`: tooling, exempt.
const TABLE: &[(&str, Option<u8>)] = &[
    ("core", Some(0)),
    ("forge", Some(1)),
    ("engine", Some(5)),
    ("ui-egui", Some(6)),
    // The rasteriser uses the craft facade without its `ui` features and no toolkit, and both ui-egui and the CLI call
    // it, so it sits beside the engine and below ui-egui: a crate may not depend on another of its own layer.
    ("share-render", Some(5)),
    ("pr-craft", Some(7)),
    ("cli", Some(7)),
    ("xtask", None),
];

/// Toolkit and windowing crates; a trailing `*` is a prefix. `craft` counts only with a `ui` feature.
const UI_CRATES: &[&str] = &["egui*", "eframe", "winit", "rfd", "wgpu*"];
const UI_MIN_LAYER: u8 = 6;

pub struct Dep {
    pub name: String,
    pub workspace: bool,
    pub ui_features: bool,
}

pub struct Crate {
    pub name: String,
    pub deps: Vec<Dep>,
}

fn layer(name: &str) -> Result<Option<u8>, String> {
    let short = name.strip_prefix("pr-craft-").unwrap_or(name);
    TABLE.iter().find(|(n, _)| *n == short).map(|(_, l)| *l).ok_or_else(|| format!("{name}: not in the layer table (xtask/src/layers.rs)"))
}

fn is_ui(dep: &Dep) -> bool {
    dep.name == "craft" && dep.ui_features
        || UI_CRATES.iter().any(|p| p.strip_suffix('*').map_or(dep.name == *p, |prefix| dep.name.starts_with(prefix)))
}

/// Every violation, one line each.
pub fn check(crates: &[Crate]) -> Vec<String> {
    let mut out = Vec::new();
    for krate in crates {
        let own = match layer(&krate.name) {
            Ok(Some(l)) => l,
            Ok(None) => continue,
            Err(e) => {
                out.push(e);
                continue;
            }
        };
        for dep in &krate.deps {
            if dep.workspace {
                match layer(&dep.name) {
                    Ok(Some(l)) if l >= own => out.push(format!("{} (L{own}) depends on {} (L{l})", krate.name, dep.name)),
                    Err(e) => out.push(e),
                    _ => {}
                }
            } else if own < UI_MIN_LAYER && is_ui(dep) {
                out.push(format!("{} (L{own}) uses the UI crate {}", krate.name, dep.name));
            }
        }
    }
    out
}

/// The model from `cargo metadata --no-deps`.
pub fn from_metadata(meta: &Value) -> Result<Vec<Crate>, String> {
    let packages = meta["packages"].as_array().ok_or("cargo metadata: no packages")?;
    let members: Vec<&str> = packages.iter().filter_map(|p| p["name"].as_str()).collect();
    Ok(packages
        .iter()
        .map(|p| Crate {
            name: p["name"].as_str().unwrap_or_default().to_string(),
            deps: p["dependencies"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|d| d["kind"].is_null()) // normal deps only; dev and build deps are free
                .map(|d| {
                    let name = d["name"].as_str().unwrap_or_default().to_string();
                    let features = d["features"].as_array().into_iter().flatten().filter_map(Value::as_str);
                    Dep { workspace: members.contains(&name.as_str()), ui_features: features.into_iter().any(|f| f.starts_with("ui")), name }
                })
                .collect(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn krate(name: &str, deps: &[(&str, bool, bool)]) -> Crate {
        Crate { name: name.into(), deps: deps.iter().map(|&(n, workspace, ui_features)| Dep { name: n.into(), workspace, ui_features }).collect() }
    }

    #[test]
    fn upward_deps_ui_below_l6_and_unknown_crates_are_violations() {
        let ok = [
            krate("pr-craft-engine", &[("pr-craft-forge", true, false), ("serde", false, false)]),
            krate("pr-craft-ui-egui", &[("pr-craft-engine", true, false), ("craft", false, true)]),
            krate("pr-craft-forge", &[("craft", false, false)]),
        ];
        assert!(check(&ok).is_empty(), "{:?}", check(&ok));
        let bad = [
            krate("pr-craft-forge", &[("pr-craft-engine", true, false)]),
            krate("pr-craft-engine", &[("craft", false, true)]),
            krate("pr-craft-core", &[("egui-winit", false, false)]),
            krate("pr-craft-stray", &[]),
        ];
        assert_eq!(check(&bad).len(), bad.len(), "{:?}", check(&bad));
    }
}
