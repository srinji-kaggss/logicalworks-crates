//! Reading the two kinds of TOML the audit judges: crate manifests
//! (`Cargo.toml`) and the approval register (`contract/APPROVED.toml`).
//!
//! The tables come from the shared TOML-subset reader; this module owns what the
//! audit makes of them: the package name, the workspace members, every declared
//! dependency, and the approvals merged per crate.

use std::collections::{BTreeMap, BTreeSet};

use super::toml_subset::{Table, list, raw, text};

/// One approval of the register.
pub(super) struct Record {
    /// The owning surface.
    pub(super) owner: Option<String>,
    /// The capability the edge provides.
    pub(super) capability: Option<String>,
    /// The admission tier.
    pub(super) tier: Option<String>,
    /// The approver's reason.
    pub(super) reason: Option<String>,
    /// The crates admitted to reach the edge.
    pub(super) consumers: BTreeSet<String>,
}

/// The text of the entry `key` of the table `path`, if present.
fn text_of(tables: &[Table], path: &str, key: &str) -> Option<String> {
    tables
        .iter()
        .filter(|table| table.path == path && !table.array)
        .find_map(|table| text(table, key))
        .map(str::to_owned)
}

/// A crate's import name: its package name, dashes to underscores.
pub(super) fn crate_name(tables: &[Table]) -> Option<String> {
    text_of(tables, "package", "name").map(|name| name.replace('-', "_"))
}

/// The members a workspace manifest lists.
pub(super) fn workspace_members(tables: &[Table]) -> Vec<String> {
    for table in tables.iter().filter(|table| table.path == "workspace") {
        if let Some(members) = list(table, "members") {
            return members.to_vec();
        }
    }
    Vec::new()
}

/// The segments of a dotted header path, with quoted segments kept whole.
fn segments(path: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in path.chars() {
        match quote {
            Some(open) => {
                if ch == open {
                    quote = None;
                } else {
                    current.push(ch);
                }
            }
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == '.' => parts.push(std::mem::take(&mut current)),
            None => current.push(ch),
        }
    }
    parts.push(current);
    parts
}

/// The dependency table kinds.
const DEPENDENCY_TABLES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

/// Every crate a manifest declares, across every dependency table.
///
/// Target-specific tables (`[target.'cfg(unix)'.dependencies]`) are included: an
/// edge is an edge whether or not this host compiles it. Both the inline form
/// (`name = { … }`) and the sub-table form (`[dependencies.name]`) name a crate.
pub(super) fn declared_dependencies(tables: &[Table]) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for table in tables {
        let parts = segments(&table.path);
        let Some(first) = parts.first() else {
            continue;
        };
        let base = if first == "target" { 2_usize } else { 0_usize };
        let Some(kind) = parts.get(base) else {
            continue;
        };
        if !DEPENDENCY_TABLES.contains(&kind.as_str()) {
            continue;
        }
        match parts.len().saturating_sub(base) {
            1 => {
                for entry in &table.entries {
                    if let Some(name) = entry.key.split('.').next() {
                        found.insert(name.replace('-', "_"));
                    }
                }
            }
            2 => {
                if let Some(name) = parts.get(base.saturating_add(1)) {
                    found.insert(name.replace('-', "_"));
                }
            }
            _ => {}
        }
    }
    found
}

/// The crates a comma-separated `allowed_consumers` names, dashes normalised.
pub(super) fn consumers_of(raw: &str) -> BTreeSet<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| name.replace('-', "_"))
        .collect()
}

/// The text or raw value of the entry `key` of an array-of-tables element.
fn field(table: &Table, key: &str) -> Option<String> {
    text(table, key)
        .or_else(|| raw(table, key))
        .map(str::to_owned)
}

/// `contract/APPROVED.toml` keyed by crate name, dashes normalised to underscores.
///
/// One crate can be approved once per owner (`tracing` is registered for
/// `lgwks_std` and again for `lgwks_ast`). Keying by name alone made the last
/// record win and silently dropped the earlier consumer, so the records are
/// merged: the first keeps its owner and every record's consumers are admitted.
pub(super) fn approvals(tables: &[Table]) -> BTreeMap<String, Record> {
    let mut merged: BTreeMap<String, Record> = BTreeMap::new();
    for table in tables
        .iter()
        .filter(|table| table.path == "approved" && table.array)
    {
        let Some(raw_name) = field(table, "crate") else {
            continue;
        };
        let name = raw_name.replace('-', "_");
        let consumers = match field(table, "allowed_consumers") {
            Some(raw) => consumers_of(&raw),
            None => BTreeSet::new(),
        };
        if let Some(existing) = merged.get_mut(&name) {
            existing.consumers.extend(consumers);
            continue;
        }
        merged.insert(
            name,
            Record {
                owner: field(table, "owner"),
                capability: field(table, "capability"),
                tier: field(table, "tier"),
                reason: field(table, "reason"),
                consumers,
            },
        );
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::{approvals, crate_name, declared_dependencies, workspace_members};
    use crate::toml_subset::read_tables;

    const MANIFEST: &str = "[package]\nname = \"lgwks-demo\"\n\n\
        [dependencies]\nserde = { version = \"1\", features = [\"derive\"] }\nlgwks_std.workspace = true\n\n\
        [dev-dependencies.proptest]\nversion = \"1\"\n\n\
        [target.'cfg(unix)'.dependencies]\nrustix = { version = \"1\" }\n\n\
        [features]\nfull = [\"a\"]\n";

    #[test]
    fn a_manifest_names_its_crate_and_every_declared_dependency() {
        let tables = read_tables(MANIFEST);
        assert_eq!(
            crate_name(&tables).as_deref(),
            Some("lgwks_demo"),
            "dashes become underscores"
        );
        assert_eq!(
            declared_dependencies(&tables)
                .into_iter()
                .collect::<Vec<_>>(),
            vec!["lgwks_std", "proptest", "rustix", "serde"],
            "inline, dotted, sub-table and target forms all count; features do not"
        );
    }

    #[test]
    fn a_workspace_lists_its_members_across_lines() {
        let tables =
            read_tables("[workspace]\nmembers = [\n  \"crates/a\", # one\n  \"crates/b\",\n]\n");
        assert_eq!(
            workspace_members(&tables),
            vec!["crates/a", "crates/b"],
            "a multi-line array"
        );
    }

    #[test]
    fn approvals_merge_per_crate_and_keep_the_first_owner() {
        let register = "[[approved]]\ncrate = \"tracing\"\nowner = \"lgwks_std\"\nreason = \"\"\"a \\\n   b\"\"\"\nallowed_consumers = \"lgwks_std\"\n\n\
            [[approved]]\ncrate = \"tracing\"\nowner = \"lgwks_ast\"\nallowed_consumers = \"lgwks-ast, lgwks_std\"\n";
        let merged = approvals(&read_tables(register));
        let record = merged.get("tracing");
        assert_eq!(
            record.map(|found| found.owner.as_deref()),
            Some(Some("lgwks_std")),
            "first owner wins"
        );
        assert_eq!(
            record.map(|found| found.consumers.iter().cloned().collect::<Vec<_>>()),
            Some(vec!["lgwks_ast".to_owned(), "lgwks_std".to_owned()]),
            "consumers are the union"
        );
        assert_eq!(
            record.and_then(|found| found.reason.clone()).as_deref(),
            Some("a b"),
            "a continued line folds"
        );
    }
}
