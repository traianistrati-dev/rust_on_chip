//! Pre-flight checks for publishing a library crate to a registry.
//!
//! # Why the IDE checks the manifest itself
//!
//! `cargo publish --dry-run` looks like the obvious gate, and it is not.
//! Verified against cargo 1.98: a manifest with no `description` and no
//! `license` passes a dry run with only
//!
//! ```text
//! warning: manifest has no description, license, license-file, documentation,
//!          homepage or repository
//! ```
//!
//! and then `warning: aborting upload due to dry run`. crates.io rejects that
//! upload SERVER-side, so a green dry run says nothing about whether the real
//! publish will be accepted. Everything crates.io requires is therefore checked
//! here, before cargo is ever launched.
//!
//! # The path-dependency rule, which is the one that bites
//!
//! A library extracted out of a project depends on its siblings by path. Cargo
//! refuses that outright:
//!
//! ```text
//! error: all dependencies must have a version requirement specified when
//!        publishing. dependency `dep` does not specify a version
//! ```
//!
//! Adding `version` next to `path` is NOT the fix on its own — cargo then goes
//! looking for the dependency on the registry:
//!
//! ```text
//! error: no matching package named `dep` found
//!        location searched: crates.io index
//! ```
//!
//! So a crate can only be published after every sibling it depends on is
//! already there. Publishing has an order, bottom-up, and saying so up front is
//! more useful than letting cargo fail twice.

/// How badly a finding stands in the way.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Severity {
    /// crates.io (or cargo) will refuse the upload.
    Blocker,
    /// Accepted, but the crate page will be poorer for it.
    Advice,
}

/// One thing wrong with the manifest.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Finding {
    pub severity: Severity,
    /// The `[package]` key at fault, when the fix is "fill this in". `None` for
    /// findings that are not a single missing field.
    pub field: Option<&'static str>,
    pub message: String,
}

/// `[package]` keys the publish dialog offers to fill in, in the order they are
/// shown. The first two are what crates.io actually requires; the rest make the
/// crate page useful.
pub const EDITABLE_FIELDS: &[(&str, &str)] = &[
    ("description", "One line describing what the crate does"),
    ("license", "SPDX expression, e.g. `MIT OR Apache-2.0`"),
    ("repository", "URL of the source repository"),
    ("homepage", "Project or documentation site"),
    ("readme", "Path to a README, e.g. `README.md`"),
    ("keywords", "Up to 5, e.g. `[\"embedded\", \"driver\"]`"),
    ("categories", "e.g. `[\"embedded\", \"no-std\"]`"),
];

/// A dependency declared with a `path`, as seen in a manifest.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PathDep {
    pub name: String,
    /// The `path` value exactly as written - relative to the manifest's own
    /// folder, `/` or `\` as the author typed it.
    pub path: String,
    /// Whether the entry also carries a `version` requirement.
    pub has_version: bool,
    /// A `[dev-dependencies]` entry. Cargo STRIPS a version-less path
    /// dev-dependency when packaging, so it does not stop a publish - reporting
    /// one as a blocker sent the user to add a `version`, which then really
    /// does fail ("no matching package named …").
    pub dev_only: bool,
}

// ── Manifest reading ─────────────────────────────────────────────────────────
// Through `toml_edit`, not a hand-rolled scanner. The first version of this
// module read the manifest line by line and an adversarial review found four
// separate ways it lied about a REAL manifest: `description.workspace = true`
// read as missing (a false blocker, then a duplicate key when written),
// a `keywords` array spread over several lines read as the value `[`,
// `[dependencies] # third-party` not recognised as a section header at all so a
// new key was appended into the WRONG table, and a `readme = "docs\README.md"`
// written unescaped, which stops the manifest parsing.
//
// `toml_edit` also preserves comments, key order and line endings on the way
// out, which matters because this writes into a file the user owns and diffs.

/// Parse, or `None` when the manifest is not valid TOML (a half-typed file in
/// the editor). Every reader below degrades to "nothing found" rather than
/// guessing.
fn doc(manifest: &str) -> Option<toml_edit::DocumentMut> {
    manifest.parse::<toml_edit::DocumentMut>().ok()
}

/// Whether `manifest` is TOML at all. Every reader here returns "nothing" for
/// one that is not, and a caller that must tell "no dependencies" apart from
/// "could not read the file" asks this first.
pub fn manifest_parses(manifest: &str) -> bool {
    doc(manifest).is_some()
}

/// A string array under `[workspace]` - `members`, `exclude` - as written.
/// Empty when the key, the table or the manifest is missing or unreadable.
pub fn workspace_array(manifest: &str, key: &str) -> Vec<String> {
    doc(manifest)
        .and_then(|d| {
            d.get("workspace")
                .and_then(|w| w.get(key))
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
        })
        .unwrap_or_default()
}

/// Whether the manifest declares a `[workspace]` table of its own - which makes
/// its package the root of its own workspace, so cargo loads it even from
/// inside another workspace's folder.
pub fn has_workspace_table(manifest: &str) -> bool {
    doc(manifest).is_some_and(|d| d.get("workspace").is_some())
}

/// Keys of the dependencies declared `{ workspace = true }` - inherited from the
/// root's `[workspace.dependencies]` - in every dependency table.
pub fn inherited_deps(manifest: &str) -> Vec<String> {
    let Some(doc) = doc(manifest) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut collect = |table: &toml_edit::Item| {
        if let Some(t) = table.as_table_like() {
            out.extend(
                t.iter()
                    .filter(|(_, item)| is_inherited(item))
                    .map(|(name, _)| name.to_owned()),
            );
        }
    };
    for name in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(t) = doc.get(name) {
            collect(t);
        }
    }
    if let Some(targets) = doc.get("target").and_then(|t| t.as_table_like()) {
        for (_, per_target) in targets.iter() {
            for name in ["dependencies", "dev-dependencies", "build-dependencies"] {
                if let Some(t) = per_target.get(name) {
                    collect(t);
                }
            }
        }
    }
    out
}

/// `(name, path)` of every entry of `table` that carries a `path`.
fn path_entries(table: Option<&toml_edit::Item>) -> Vec<(String, String)> {
    let Some(t) = table.and_then(|t| t.as_table_like()) else {
        return Vec::new();
    };
    t.iter()
        .filter_map(|(name, item)| {
            let path = item.get("path")?.as_str()?;
            Some((name.to_owned(), path.to_owned()))
        })
        .collect()
}

/// `[workspace.dependencies]` entries that carry a `path`, as `(name, path)` -
/// the path relative to THIS (the root) manifest.
pub fn workspace_path_deps(manifest: &str) -> Vec<(String, String)> {
    doc(manifest)
        .map(|d| path_entries(d.get("workspace").and_then(|w| w.get("dependencies"))))
        .unwrap_or_default()
}

/// `[patch.<registry>]` entries that carry a `path`, as `(name, path)`: the
/// local folder cargo builds IN PLACE of that registry crate, for every crate
/// of the workspace that depends on it.
pub fn patch_path_deps(manifest: &str) -> Vec<(String, String)> {
    let Some(doc) = doc(manifest) else {
        return Vec::new();
    };
    let Some(patch) = doc.get("patch").and_then(|p| p.as_table_like()) else {
        return Vec::new();
    };
    patch
        .iter()
        .flat_map(|(_, registry)| path_entries(Some(registry)))
        .collect()
}

/// Every REGISTRY dependency in `[dependencies]` (and the per-target tables),
/// with its version requirement: `name = "0.1"` and `name = { version = .. }`,
/// but never one that carries a `path` or a `git` - those are not the
/// registry's copy, whatever they are called.
pub fn registry_deps(manifest: &str) -> Vec<(String, String)> {
    let Some(doc) = doc(manifest) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut collect = |table: &toml_edit::Item| {
        let Some(t) = table.as_table_like() else {
            return;
        };
        for (name, item) in t.iter() {
            let version = if let Some(v) = item.as_str() {
                Some(v)
            } else if item.get("path").is_none() && item.get("git").is_none() {
                item.get("version").and_then(|v| v.as_str())
            } else {
                None
            };
            if let Some(v) = version {
                out.push((name.to_owned(), v.to_owned()));
            }
        }
    };
    if let Some(t) = doc.get("dependencies") {
        collect(t);
    }
    if let Some(targets) = doc.get("target").and_then(|t| t.as_table_like()) {
        for (_, per_target) in targets.iter() {
            if let Some(t) = per_target.get("dependencies") {
                collect(t);
            }
        }
    }
    out
}

/// Is `key` present in `[package]` with a usable value?
///
/// An empty string counts as ABSENT: the extract dialog writes
/// `description = ""` when the field is left blank, and crates.io rejects that
/// exactly as it rejects a missing key.
///
/// A workspace-inherited field (`description.workspace = true`) counts as
/// PRESENT — the value lives in the workspace root, and reporting it as missing
/// was a false blocker on every crate that inherits.
pub fn package_field(manifest: &str, key: &str) -> Option<String> {
    let doc = doc(manifest)?;
    let item = doc.get("package")?.get(key)?;
    if is_inherited(item) {
        return Some("(inherited from the workspace)".to_owned());
    }
    if let Some(s) = item.as_str() {
        let s = s.trim();
        return (!s.is_empty()).then(|| s.to_owned());
    }
    if let Some(arr) = item.as_array() {
        // Rendered from the ARRAY, not from the item: an item's `to_string`
        // carries its decor, so a trailing comment would come along with it.
        return (!arr.is_empty()).then(|| arr.to_string().trim().to_owned());
    }
    // `publish = false` is the one non-string key any caller asks about, and
    // it too must arrive without the `# internal only` someone wrote after it.
    if let Some(b) = item.as_bool() {
        return Some(b.to_string());
    }
    if let Some(i) = item.as_integer() {
        return Some(i.to_string());
    }
    None
}

/// Is `[package] <key>` inherited from `[workspace.package]`?
///
/// Callers that offer the field for EDITING need this: there is no literal
/// value here to change, and [`package_field`]'s answer for one is a sentence
/// for display, not text to put in a box.
pub fn is_workspace_inherited(manifest: &str, key: &str) -> bool {
    doc(manifest)
        .and_then(|d| d.get("package").and_then(|p| p.get(key)).map(is_inherited))
        .unwrap_or(false)
}

/// `key.workspace = true` — the value is defined in `[workspace.package]`.
fn is_inherited(item: &toml_edit::Item) -> bool {
    item.get("workspace")
        .and_then(|w| w.as_bool())
        .unwrap_or(false)
}

/// Every dependency that carries a `path`, across all three dependency tables
/// AND the per-target ones (`[target.'cfg(…)'.dependencies]`), which cargo
/// treats identically for publishing.
pub fn path_deps(manifest: &str) -> Vec<PathDep> {
    let Some(doc) = doc(manifest) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut collect = |table: &toml_edit::Item, dev_only: bool| {
        let Some(t) = table.as_table_like() else {
            return;
        };
        for (name, item) in t.iter() {
            if let Some(path) = item.get("path") {
                out.push(PathDep {
                    name: name.to_owned(),
                    path: path.as_str().unwrap_or_default().to_owned(),
                    has_version: item.get("version").is_some(),
                    dev_only,
                });
            }
        }
    };
    const TABLES: [(&str, bool); 3] = [
        ("dependencies", false),
        ("dev-dependencies", true),
        ("build-dependencies", false),
    ];
    for (name, dev) in TABLES {
        if let Some(t) = doc.get(name) {
            collect(t, dev);
        }
    }
    // `[target.<triple or cfg>.dependencies]`
    if let Some(targets) = doc.get("target").and_then(|t| t.as_table_like()) {
        for (_, per_target) in targets.iter() {
            for (name, dev) in TABLES {
                if let Some(t) = per_target.get(name) {
                    collect(t, dev);
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.dedup_by(|a, b| a.name == b.name);
    out
}

/// A `path` dependency with what decides whether cargo resolves it for the
/// package: `optional`, and whether it sits under a `[target.<spec>]` table.
/// One inherited with `{ workspace = true }` carries the path of its
/// `[workspace.dependencies]` entry.
pub struct PathDepUse {
    pub name: String,
    /// As written - relative to the manifest's own folder.
    pub path: String,
    pub optional: bool,
    pub per_target: bool,
}

/// Every `path` dependency of `manifest`, in all three dependency tables and
/// the per-target ones, direct or inherited from `[workspace.dependencies]`.
pub fn path_dep_uses(manifest: &str) -> Vec<PathDepUse> {
    let Some(doc) = doc(manifest) else {
        return Vec::new();
    };
    let inheritable = path_entries(doc.get("workspace").and_then(|w| w.get("dependencies")));
    let mut out = Vec::new();
    let mut collect = |table: &toml_edit::Item, per_target: bool| {
        let Some(t) = table.as_table_like() else {
            return;
        };
        for (name, item) in t.iter() {
            let path = if let Some(p) = item.get("path").and_then(|p| p.as_str()) {
                p.to_owned()
            } else if is_inherited(item)
                && let Some((_, p)) = inheritable.iter().find(|(n, _)| n == name)
            {
                p.clone()
            } else {
                continue;
            };
            out.push(PathDepUse {
                name: name.to_owned(),
                path,
                optional: item
                    .get("optional")
                    .and_then(|o| o.as_bool())
                    .unwrap_or(false),
                per_target,
            });
        }
    };
    const TABLES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];
    for name in TABLES {
        if let Some(t) = doc.get(name) {
            collect(t, false);
        }
    }
    if let Some(targets) = doc.get("target").and_then(|t| t.as_table_like()) {
        for (_, per_target) in targets.iter() {
            for name in TABLES {
                if let Some(t) = per_target.get(name) {
                    collect(t, true);
                }
            }
        }
    }
    out
}

/// Whether resolving `manifest` with its default features takes in the
/// optional dependency `dep`: through `dep`, `dep:dep`, `dep/<feature>` or
/// `dep?/<feature>`, directly or in a feature `default` enables. A weak
/// `dep?/<feature>` counts: cargo's dependency resolver - so `cargo metadata`,
/// Cargo.lock and rust-analyzer - takes the dependency in, although the build
/// then compiles nothing of it (measured with cargo 1.98). A feature of the
/// same name shadows the dependency's implicit one.
pub fn enabled_by_default(manifest: &str, dep: &str) -> bool {
    let Some(doc) = doc(manifest) else {
        return false;
    };
    let Some(features) = doc.get("features").and_then(|f| f.as_table_like()) else {
        return false;
    };
    let mut seen: Vec<String> = Vec::new();
    let mut todo = vec!["default".to_owned()];
    while let Some(feature) = todo.pop() {
        if seen.contains(&feature) {
            continue;
        }
        seen.push(feature.clone());
        let Some(list) = features.get(&feature).and_then(|v| v.as_array()) else {
            continue;
        };
        for entry in list.iter().filter_map(|v| v.as_str()) {
            if let Some(name) = entry.strip_prefix("dep:") {
                if name == dep {
                    return true;
                }
            } else if let Some((name, _)) = entry.split_once('/') {
                if name.strip_suffix('?').unwrap_or(name) == dep {
                    return true;
                }
            } else if features.contains_key(entry) {
                todo.push(entry.to_owned());
            } else if entry == dep {
                return true;
            }
        }
    }
    false
}

/// Check a library's manifest for everything that would stop a publish.
///
/// `sibling_published` answers whether a path dependency is already available
/// on the target registry. The caller cannot always know — pass `None` for
/// "unknown" and the finding is worded as a caution rather than a refusal.
pub fn check_manifest(
    manifest: &str,
    sibling_published: impl Fn(&str) -> Option<bool>,
) -> Vec<Finding> {
    let mut out = Vec::new();

    // Before anything else: is this a crate at all? A manifest that does not
    // parse, or one that is a virtual workspace with no `[package]`, has
    // nothing to publish - and every check below would report it as a crate
    // missing seven fields, which is the wrong sentence entirely.
    match doc(manifest) {
        None => {
            out.push(Finding {
                severity: Severity::Blocker,
                field: None,
                message: "Cargo.toml does not parse as TOML - fix it in the editor first."
                    .to_owned(),
            });
            return out;
        }
        Some(d) if d.get("package").and_then(|p| p.as_table_like()).is_none() => {
            out.push(Finding {
                severity: Severity::Blocker,
                field: None,
                message: "no `[package]` table - this Cargo.toml describes a workspace, not a \
                          crate. There is nothing here to upload."
                    .to_owned(),
            });
            return out;
        }
        Some(_) => {}
    }

    // `publish = false` is a deliberate "never upload this" and outranks
    // everything else worth saying.
    if package_field(manifest, "publish").as_deref() == Some("false") {
        out.push(Finding {
            severity: Severity::Blocker,
            field: None,
            message: "`publish = false` in Cargo.toml - this crate is marked as never publishable."
                .to_owned(),
        });
    }

    if package_field(manifest, "description").is_none() {
        out.push(Finding {
            severity: Severity::Blocker,
            field: Some("description"),
            message: "crates.io requires a `description`. `cargo publish --dry-run` only WARNS \
                      about this - the upload is refused by the server."
                .to_owned(),
        });
    }
    if package_field(manifest, "license").is_none()
        && package_field(manifest, "license-file").is_none()
    {
        out.push(Finding {
            severity: Severity::Blocker,
            field: Some("license"),
            message: "crates.io requires `license` or `license-file`. A dry run only warns."
                .to_owned(),
        });
    }

    for dep in path_deps(manifest) {
        let name = &dep.name;
        // Verified with cargo 1.98: a crate with a version-less path
        // dev-dependency packages and dry-runs cleanly - cargo drops the entry.
        if dep.dev_only {
            continue;
        }
        if !dep.has_version {
            out.push(Finding {
                severity: Severity::Blocker,
                field: None,
                message: format!(
                    "`{name}` is a path dependency with no `version` - cargo refuses to publish \
                     (\"all dependencies must have a version requirement specified\"). Add \
                     `version = \"…\"` next to its `path`."
                ),
            });
            continue;
        }
        match sibling_published(name) {
            Some(true) => {}
            Some(false) => out.push(Finding {
                severity: Severity::Blocker,
                field: None,
                message: format!(
                    "`{name}` is not on the registry yet. A version next to the path is not \
                     enough - cargo looks the dependency up and fails with \"no matching package \
                     named `{name}` found\". Publish `{name}` first."
                ),
            }),
            None => out.push(Finding {
                severity: Severity::Advice,
                field: None,
                message: format!(
                    "`{name}` is a path dependency. It must already exist on the registry, or \
                     the publish fails with \"no matching package named `{name}` found\"."
                ),
            }),
        }
    }

    for (key, _) in EDITABLE_FIELDS.iter().skip(2) {
        if package_field(manifest, key).is_none() {
            out.push(Finding {
                severity: Severity::Advice,
                field: Some(key),
                message: format!("no `{key}` - accepted, but the crate page will be poorer."),
            });
        }
    }

    out.sort_by_key(|f| f.severity);
    out
}

/// Set `key` to `value` in the manifest's `[package]` section, preserving the
/// rest of the file byte for byte.
///
/// `value` is typed, not spliced: a `keywords` / `categories` entry becomes a
/// real TOML array, everything else a string that `toml_edit` escapes. The
/// first version rendered the text verbatim whenever it started with `[` or
/// `"`, so a Windows path (`docs\README.md`) or a stray bracket stopped the
/// manifest parsing.
///
/// Returns `None` when the manifest is not valid TOML — nothing is written over
/// a file that cannot be read back.
pub fn set_package_field(manifest: &str, key: &str, value: &str) -> Option<String> {
    let mut doc = manifest.parse::<toml_edit::DocumentMut>().ok()?;
    let value = value.trim();
    let item = if matches!(key, "keywords" | "categories") {
        let mut arr = toml_edit::Array::new();
        for part in split_list(value) {
            arr.push(part);
        }
        toml_edit::value(arr)
    } else {
        toml_edit::value(value)
    };
    // `doc["package"][key] = …` would CREATE the table when it is missing, and
    // a manifest without one is not a crate that forgot a field - it is a
    // virtual workspace. Writing there produces `package = { description = … }`
    // above the `[workspace]` section: a package with no `name`, which cargo
    // then refuses to load, taking the build and rust-analyzer with it.
    if !has_package_table(manifest) {
        return None;
    }
    doc["package"][key] = item;
    Some(doc.to_string())
}

/// Does the manifest parse as TOML at all?
///
/// Asked separately from [`has_package_table`], which answers `false` for both
/// "does not parse" and "parses, but is a workspace" — two different problems
/// with two different fixes, and telling a half-typed file that it describes a
/// workspace sends the reader looking for the wrong one.
pub fn parses(manifest: &str) -> bool {
    doc(manifest).is_some()
}

/// Does this manifest actually describe a package?
///
/// A virtual workspace manifest is `[workspace]` and nothing else. It parses,
/// it sits at `<crate>/Cargo.toml`, and every `package_field` lookup in it
/// answers `None` exactly like a crate that is merely missing its metadata —
/// which is why this has to be asked separately.
pub fn has_package_table(manifest: &str) -> bool {
    doc(manifest).is_some_and(|d| d.get("package").and_then(|p| p.as_table_like()).is_some())
}

/// Split what the user typed into list entries.
///
/// Accepts the TOML they might paste (`["a", "b"]`) and the plain list they are
/// far more likely to type (`a, b`) — the field is a text box, and rejecting
/// the obvious spelling would be a trap.
fn split_list(value: &str) -> Vec<String> {
    value
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|p| p.trim().trim_matches('"').trim().to_owned())
        .filter(|p| !p.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The template this IDE writes for a new library crate.
    const TEMPLATE: &str = "\
[package]
name        = \"mw_radar\"
version     = \"0.1.0\"
edition     = \"2021\"
license     = \"MIT OR Apache-2.0\"
description = \"\"
# Fill these in before `cargo publish`:
# repository  = \"https://github.com/x\"
# readme      = \"README.md\"

[dependencies]
";

    fn unknown(_: &str) -> Option<bool> {
        None
    }

    #[test]
    fn an_empty_value_counts_as_missing() {
        // The extract dialog writes `description = ""` when left blank, and
        // crates.io refuses that exactly as it refuses a missing key.
        assert_eq!(package_field(TEMPLATE, "description"), None);
        assert_eq!(
            package_field(TEMPLATE, "license"),
            Some("MIT OR Apache-2.0".to_owned())
        );
    }

    #[test]
    fn a_commented_placeholder_is_not_a_value() {
        assert_eq!(package_field(TEMPLATE, "repository"), None);
        assert_eq!(package_field(TEMPLATE, "readme"), None);
    }

    /// Found by review: a line-based reader saw the key `description.workspace`,
    /// which never equals `description`, so every crate inheriting from its
    /// workspace showed two false blockers — and Write then added a duplicate.
    #[test]
    fn a_workspace_inherited_field_is_present_not_missing() {
        let m = "[package]\nname = \"a\"\nversion.workspace = true\ndescription.workspace = true\nlicense.workspace = true\n";
        assert!(package_field(m, "description").is_some(), "inherited");
        assert!(package_field(m, "license").is_some());
        let f = check_manifest(m, unknown);
        assert!(
            f.iter().all(|x| x.severity != Severity::Blocker),
            "inheritance is not a blocker: {f:#?}"
        );
    }

    /// Found by review: a line-based read returned the single character `[`,
    /// and Write then replaced only the array's first line.
    #[test]
    fn a_multi_line_array_is_read_whole() {
        let m = "[package]\nname = \"a\"\nkeywords = [\n  \"embedded\",\n  \"driver\",\n]\n";
        let v = package_field(m, "keywords").expect("present");
        assert!(v.contains("embedded") && v.contains("driver"), "{v}");
    }

    /// Found by review: `[dependencies] # third-party` was not recognised as a
    /// header, so a new key was appended into the WRONG table.
    #[test]
    fn a_commented_section_header_is_still_a_header() {
        let m = "[package]\nname = \"a\"\n\n[dependencies] # third-party\nserde = \"1\"\n";
        let out = set_package_field(m, "description", "d").expect("valid toml");
        let pkg = out.split("[dependencies]").next().unwrap();
        assert!(pkg.contains("description = \"d\""), "{out}");
    }

    /// Found by review: a trailing comment was swallowed into the value, so the
    /// `publish = false` guard never fired.
    #[test]
    fn a_trailing_comment_is_not_part_of_the_value() {
        let m = "[package]\nname = \"a\"\ndescription = \"d\"\nlicense = \"MIT\"\npublish = false # internal only\n";
        assert_eq!(package_field(m, "publish").as_deref(), Some("false"));
        assert!(
            check_manifest(m, unknown)
                .iter()
                .any(|f| f.message.contains("publish = false"))
        );
    }

    /// Found by review: a quoted key read as absent, and Write duplicated it.
    #[test]
    fn a_quoted_key_is_the_same_key() {
        let m = "[package]\nname = \"a\"\n\"description\" = \"a driver\"\n";
        assert_eq!(package_field(m, "description").as_deref(), Some("a driver"));
    }

    /// Found by review: a Windows path went into a basic string unescaped and
    /// the manifest stopped parsing.
    #[test]
    fn a_backslash_in_a_value_cannot_break_the_manifest() {
        let out = set_package_field(TEMPLATE, "readme", "docs\\README.md").expect("valid");
        let back = out
            .parse::<toml_edit::DocumentMut>()
            .expect("still parses after the write");
        assert_eq!(back["package"]["readme"].as_str(), Some("docs\\README.md"));
    }

    #[test]
    fn a_quote_in_a_value_cannot_break_the_manifest() {
        let out = set_package_field(TEMPLATE, "description", "a \"radar\" driver").expect("valid");
        let back = out.parse::<toml_edit::DocumentMut>().expect("still parses");
        assert_eq!(
            back["package"]["description"].as_str(),
            Some("a \"radar\" driver")
        );
    }

    /// A list field becomes a real array, whichever way the user typed it — the
    /// box is free text, so the obvious spelling must not be a trap.
    #[test]
    fn a_list_field_becomes_a_toml_array() {
        for typed in ["embedded, driver", "[\"embedded\", \"driver\"]"] {
            let out = set_package_field(TEMPLATE, "keywords", typed).expect("valid");
            let back = out.parse::<toml_edit::DocumentMut>().expect("parses");
            let arr = back["package"]["keywords"].as_array().expect("array");
            let got: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
            assert_eq!(got, ["embedded", "driver"], "typed {typed:?} -> {out}");
        }
    }

    /// The file belongs to the user: a write must leave everything else alone.
    #[test]
    fn writing_a_field_preserves_the_rest_of_the_manifest() {
        let out = set_package_field(TEMPLATE, "description", "a radar driver").expect("valid");
        assert!(
            out.contains("# Fill these in before"),
            "comments kept:\n{out}"
        );
        assert!(out.contains("name        = \"mw_radar\""), "spacing kept");
        assert!(out.contains("[dependencies]"));
        let back = out.parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(
            back["package"]["description"].as_str(),
            Some("a radar driver")
        );
    }

    #[test]
    fn an_unparseable_manifest_is_never_written_over() {
        assert_eq!(
            set_package_field("[package\nname =", "description", "d"),
            None
        );
    }

    #[test]
    fn the_two_crates_io_requirements_are_blockers() {
        let found = check_manifest(TEMPLATE, unknown);
        let blockers: Vec<&Finding> = found
            .iter()
            .filter(|f| f.severity == Severity::Blocker)
            .collect();
        assert_eq!(blockers.len(), 1, "{found:#?}");
        assert_eq!(blockers[0].field, Some("description"));
    }

    #[test]
    fn a_missing_license_is_a_blocker_too() {
        let m = "[package]\nname = \"a\"\ndescription = \"d\"\n";
        assert!(
            check_manifest(m, unknown)
                .iter()
                .any(|f| f.field == Some("license") && f.severity == Severity::Blocker)
        );
        let m2 = "[package]\nname = \"a\"\ndescription = \"d\"\nlicense-file = \"LICENSE\"\n";
        assert!(
            check_manifest(m2, unknown)
                .iter()
                .all(|f| f.field != Some("license"))
        );
    }

    /// The two other ways a manifest links a local crate, read the way cargo
    /// reads them - and neither is a plain `path` dependency.
    #[test]
    fn inherited_and_patched_path_links_are_read() {
        let m = "[package]\nname = \"fw\"\n\n[workspace]\nmembers = [\"a\"]\n\n[workspace.dependencies]\na = { path = \"a\" }\nreg = \"1\"\n\n[dependencies]\na = { workspace = true }\nb = \"0.1\"\n\n[patch.crates-io]\nb = { path = \"vendor/b\" }\n";
        assert_eq!(inherited_deps(m), vec!["a".to_owned()]);
        assert_eq!(
            workspace_path_deps(m),
            vec![("a".to_owned(), "a".to_owned())]
        );
        assert_eq!(
            patch_path_deps(m),
            vec![("b".to_owned(), "vendor/b".to_owned())]
        );
        assert!(path_deps(m).is_empty(), "none of these is a plain path dep");
    }

    #[test]
    fn workspace_arrays_and_tables_are_read() {
        let m = "[package]\nname = \"a\"\n\n[workspace]\nmembers = [\"x\"]\nexclude = [\"lib\", \"./other/\"]\n";
        assert_eq!(
            workspace_array(m, "exclude"),
            vec!["lib".to_owned(), "./other/".to_owned()]
        );
        assert_eq!(workspace_array(m, "members"), vec!["x".to_owned()]);
        assert!(workspace_array("[package]\nname = \"a\"\n", "exclude").is_empty());
        assert!(has_workspace_table(m));
        assert!(!has_workspace_table("[package]\nname = \"a\"\n"));
    }

    /// A registry dependency is found in both spellings, and a `path` or `git`
    /// entry is never one - even when it names a version too.
    #[test]
    fn registry_dependencies_carry_their_version() {
        let m = "[package]\nname=\"a\"\n\n[dependencies]\nhmmd_mmwave_sensor_async = \"0.1.0\"\nssd1306 = { version = \"0.10.0\", features = [\"async\"] }\nlocal = { path = \"x\", version = \"1\" }\nforked = { git = \"https://x\" }\n";
        assert_eq!(
            registry_deps(m),
            vec![
                ("hmmd_mmwave_sensor_async".to_owned(), "0.1.0".to_owned()),
                ("ssd1306".to_owned(), "0.10.0".to_owned()),
            ]
        );
        assert!(registry_deps("not [toml").is_empty());
        assert!(!manifest_parses("not [toml"));
        assert!(manifest_parses(m));
    }

    #[test]
    fn path_dependencies_are_found_in_both_spellings() {
        let inline = "[package]\nname=\"a\"\n\n[dependencies]\ndep = { path = \"../dep\" }\n";
        assert_eq!(
            path_deps(inline),
            vec![PathDep {
                name: "dep".to_owned(),
                path: "../dep".to_owned(),
                has_version: false,
                dev_only: false
            }]
        );
        let section = "[package]\nname=\"a\"\n\n[dependencies.mw_radar]\npath = \"mw_radar\"\n";
        assert_eq!(path_deps(section).len(), 1);
        let both = "[package]\nname=\"a\"\n\n[dependencies]\ndep = { path = \"../dep\", version = \"0.1\" }\n";
        assert!(path_deps(both)[0].has_version);
    }

    /// Found by review: a `contains("path")` substring test called all three of
    /// these path dependencies.
    #[test]
    fn an_ordinary_registry_dependency_is_not_a_path_dep() {
        let m = "[package]\nname=\"a\"\n\n[dependencies]\nserde = { version = \"1\" } # switch to { path = \"../serde\" } for local dev\nfoo = { git = \"https://github.com/x/path-utils\", branch = \"main\" }\nbar = { version = \"1\", package = \"bar-pathfinder\" }\nembedded-hal = \"1.0\"\n";
        assert!(path_deps(m).is_empty(), "{:?}", path_deps(m));
        let sec = "[package]\nname=\"a\"\n\n[dependencies.serde]\nversion = \"1\"\n";
        assert!(path_deps(sec).is_empty());
    }

    /// Found by review: a per-target path dependency was invisible, so the
    /// window called the crate ready when cargo would refuse it.
    #[test]
    fn a_per_target_path_dependency_is_found_too() {
        let m = "[package]\nname=\"a\"\n\n[target.'cfg(target_os = \"none\")'.dependencies]\nmw_radar = { path = \"../mw_radar\" }\n";
        assert_eq!(
            path_deps(m),
            vec![PathDep {
                name: "mw_radar".to_owned(),
                path: "../mw_radar".to_owned(),
                has_version: false,
                dev_only: false
            }]
        );
    }

    /// Cargo fails twice on this, for two different reasons. Both are worth
    /// saying before it is launched.
    #[test]
    fn a_path_dep_is_a_blocker_whether_or_not_it_has_a_version() {
        let no_ver = "[package]\nname=\"a\"\ndescription=\"d\"\nlicense=\"MIT\"\n\n[dependencies]\ndep = { path = \"../dep\" }\n";
        assert!(
            check_manifest(no_ver, unknown)
                .iter()
                .any(|x| x.severity == Severity::Blocker && x.message.contains("no `version`"))
        );
        let with_ver = "[package]\nname=\"a\"\ndescription=\"d\"\nlicense=\"MIT\"\n\n[dependencies]\ndep = { path = \"../dep\", version = \"0.1\" }\n";
        assert!(
            check_manifest(with_ver, |_| Some(false))
                .iter()
                .any(|x| x.message.contains("Publish `dep` first"))
        );
        assert!(
            check_manifest(with_ver, |_| Some(true))
                .iter()
                .all(|x| x.severity != Severity::Blocker)
        );
    }

    /// A version-less path DEV-dependency is not a publish blocker: cargo
    /// strips it when packaging (verified with cargo 1.98). Reporting one sent
    /// the user to add a `version`, which then really does fail.
    #[test]
    fn a_dev_only_path_dependency_is_not_a_blocker() {
        let m = "[package]
name=\"a\"
description=\"d\"
license=\"MIT\"

[dev-dependencies]
helper = { path = \"../helper\" }
";
        let deps = path_deps(m);
        assert_eq!(deps.len(), 1);
        assert!(deps[0].dev_only);
        assert!(
            check_manifest(m, unknown)
                .iter()
                .all(|f| f.severity != Severity::Blocker),
            "{:#?}",
            check_manifest(m, unknown)
        );
        // …while a real dependency in the same shape still is one.
        let real = "[package]
name=\"a\"
description=\"d\"
license=\"MIT\"

[dependencies]
helper = { path = \"../helper\" }
";
        assert!(
            check_manifest(real, unknown)
                .iter()
                .any(|f| f.severity == Severity::Blocker)
        );
    }

    /// An inherited field has no literal value to edit — the dialog must not
    /// seed a text box with the display sentence and let it be written back.
    #[test]
    fn inheritance_is_visible_to_the_editor() {
        let m = "[package]
name = \"a\"
description.workspace = true
";
        assert!(is_workspace_inherited(m, "description"));
        assert!(!is_workspace_inherited(m, "name"));
        assert!(
            !is_workspace_inherited(m, "license"),
            "absent is not inherited"
        );
    }

    #[test]
    fn publish_false_is_a_blocker() {
        let m = "[package]\nname=\"a\"\ndescription=\"d\"\nlicense=\"MIT\"\npublish = false\n";
        assert!(
            check_manifest(m, unknown)
                .iter()
                .any(|x| x.severity == Severity::Blocker && x.message.contains("publish = false"))
        );
    }

    /// A cloned library can be a virtual workspace: `[workspace]` and nothing
    /// else. Every `package_field` lookup in it answers `None`, exactly like a
    /// crate that merely forgot its metadata - so without this it was reported
    /// as a crate missing a description, and writing that description created
    /// `package = { description = "…" }` above the `[workspace]` section: a
    /// package with no `name`, which cargo refuses to load.
    const VIRTUAL_WORKSPACE: &str =
        "[workspace]\nmembers = [\"core\", \"macros\"]\nresolver = \"2\"\n";

    #[test]
    fn a_virtual_workspace_is_not_a_crate_to_publish() {
        let out = check_manifest(VIRTUAL_WORKSPACE, |_| None);
        assert_eq!(
            out.len(),
            1,
            "one sentence, not seven missing fields: {out:?}"
        );
        assert_eq!(out[0].severity, Severity::Blocker);
        assert!(
            out[0].message.contains("[package]"),
            "it has to name what is missing: {}",
            out[0].message
        );
    }

    #[test]
    fn writing_a_field_never_invents_a_package_table() {
        assert!(!has_package_table(VIRTUAL_WORKSPACE));
        assert_eq!(
            set_package_field(VIRTUAL_WORKSPACE, "description", "a radar driver"),
            None,
            "refused, so the manifest cannot be corrupted into a nameless package"
        );
    }

    #[test]
    fn an_unparseable_manifest_says_so_rather_than_listing_missing_fields() {
        let out = check_manifest("[package\nname = \"radar\"\n", |_| None);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(
            out[0].message.contains("does not parse"),
            "{}",
            out[0].message
        );
    }

    #[test]
    fn a_real_package_table_is_still_recognised() {
        assert!(has_package_table("[package]\nname = \"radar\"\n"));
        // `[package]` written as an inline table is legal TOML and still a
        // package - `as_table_like` is what accepts both spellings.
        assert!(has_package_table("package = { name = \"radar\" }\n"));
    }
}
