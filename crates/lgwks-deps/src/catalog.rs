//! The capability catalog: one row per capability this repository owns, read
//! from `contract/CATALOG.toml`, rendered as the page `docs/catalog.md` and as
//! the JSON `lgwks-deps catalog --json` prints (#732).
//!
//! A capability that exists but cannot be found is a capability that gets
//! rebuilt by hand, and the hand-built copy is what the std-first gate refuses.
//! The catalog closes that loop from both ends: it is the page a person or an
//! agent reads, and it is the register the gate is checked against, so an owner
//! the gate can name but the catalog cannot is a gate defect rather than a
//! silent dead end.
//!
//! The rows are the source of truth. `docs/catalog.md` is generated from them
//! and never edited, so a capability whose owner moves is a one-line change
//! here plus a re-render the `catalog` lane proves.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[path = "bin/shared/toml_subset.rs"]
mod toml_subset;

use toml_subset::{Table, list, raw, read_tables, text};

/// The register this catalog is read from.
pub const CATALOG_PATH: &str = "contract/CATALOG.toml";

/// The generated page.
pub const DOC_PATH: &str = "docs/catalog.md";

/// The source files the gate's own refusals name owners in. Every `lgwks_*`
/// path named in one of them must have a catalog row: a refusal that points a
/// reader at an owner no catalog lists is the dead end #732 is about.
const GATE_SOURCES: [&str; 3] = [
    "crates/lgwks-deps/src/bin/check_std_first/scans.rs",
    "crates/lgwks-deps/src/lib.rs",
    "crates/lgwks-deps/src/refusal.rs",
];

/// The publishing crates, plus the repository itself for a documentation row.
const CRATES: [&str; 6] = [
    "lgwks_std",
    "lgwks_bot",
    "lgwks_ast",
    "lgwks_deps",
    "lgwks_macros",
    "repository",
];

/// Why the catalog could not be read or checked.
#[derive(Debug)]
#[non_exhaustive]
pub enum CatalogError {
    /// A file could not be read or written.
    Io {
        /// What was being touched.
        path: PathBuf,
        /// What the operating system said.
        cause: io::Error,
    },
    /// A row is missing a field the contract requires.
    Missing {
        /// The capability's name, or its index when it has none.
        row: String,
        /// The field that is absent.
        field: &'static str,
    },
    /// A field holds a value the contract does not define.
    BadValue {
        /// The capability's name.
        row: String,
        /// The field.
        field: &'static str,
        /// The value as written.
        value: String,
    },
    /// A field the contract does not define.
    UnknownField {
        /// The capability's name.
        row: String,
        /// The field as written.
        field: String,
    },
    /// The register declares no capability.
    Empty,
    /// A formatted write into the rendered page failed, which a `String` cannot.
    Render {
        /// Why the formatter refused.
        cause: fmt::Error,
    },
}

impl fmt::Display for CatalogError {
    /// Names the row and the field.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Io {
                ref path,
                ref cause,
            } => write!(formatter, "{}: {cause}", path.display()),
            Self::Missing { ref row, field } => {
                write!(formatter, "{CATALOG_PATH}: {row} has no `{field}`")
            }
            Self::BadValue {
                ref row,
                field,
                ref value,
            } => write!(
                formatter,
                "{CATALOG_PATH}: {row} has `{field} = {value}`, which is not one of the defined values"
            ),
            Self::UnknownField { ref row, ref field } => write!(
                formatter,
                "{CATALOG_PATH}: {row} has an unknown field `{field}`"
            ),
            Self::Empty => write!(formatter, "{CATALOG_PATH} declares no capability"),
            Self::Render { ref cause } => write!(formatter, "rendering failed: {cause}"),
        }
    }
}

impl std::error::Error for CatalogError {
    /// The formatter's own error, when rendering failed.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Render { ref cause } => Some(cause),
            Self::Io { .. }
            | Self::Missing { .. }
            | Self::BadValue { .. }
            | Self::UnknownField { .. }
            | Self::Empty => None,
        }
    }
}

/// One capability and its owner.
#[derive(Debug)]
pub struct Capability {
    /// The capability as a person or an agent searches for it.
    name: String,
    /// The Rust path that owns it; absent for a documentation capability.
    path: Option<String>,
    /// The file that implements it.
    file: String,
    /// The publishing crate, or `repository`.
    krate: String,
    /// The cargo feature that carries it, or `always`.
    feature: String,
    /// Whether it is on by default.
    default_on: bool,
    /// Why a capability is off; required when it is.
    reason: Option<String>,
    /// The spellings a search may use.
    words: Vec<String>,
    /// What an agent writes when it cannot find the owner.
    replaces: Vec<String>,
}

impl Capability {
    /// The capability as a person or an agent searches for it.
    #[must_use]
    pub(crate) fn name(&self) -> &str {
        self.name.as_str()
    }

    /// The publishing crate, or `repository`.
    #[must_use]
    pub(crate) fn krate(&self) -> &str {
        self.krate.as_str()
    }

    /// Whether a consumer that names no features gets this capability: the
    /// column the whole catalog exists to answer, read by every renderer.
    #[must_use]
    pub(crate) fn default_on(&self) -> bool {
        self.default_on
    }

    /// Why a capability is off, when it is.
    #[must_use]
    pub(crate) fn reason(&self) -> Option<&str> {
        match self.reason {
            Some(ref reason) => Some(reason.as_str()),
            None => None,
        }
    }

    /// The owner as one string, for a row that has no Rust path.
    #[must_use]
    fn owner(&self) -> &str {
        match self.path {
            Some(ref path) => path,
            None => self.file.as_str(),
        }
    }
}

/// Every capability, in the order the register lists them.
#[derive(Debug)]
pub struct Catalog {
    /// The rows.
    rows: Vec<Capability>,
}

impl Catalog {
    /// The rows, in the order the register lists them.
    #[must_use]
    pub(crate) fn rows(&self) -> &[Capability] {
        self.rows.as_slice()
    }

    /// How many capabilities the register declares, for the one-line summary
    /// each command prints after a successful read.
    #[must_use]
    pub fn capabilities(&self) -> usize {
        self.rows.len()
    }
}

/// The one string field of a row, or a refusal naming the field.
fn required(table: &Table, field: &'static str, row: &str) -> Result<String, CatalogError> {
    match text(table, field) {
        Some(value) => Ok(value.to_owned()),
        None => Err(CatalogError::Missing {
            row: row.to_owned(),
            field,
        }),
    }
}

/// The string array under `key`, empty when the row does not write one.
fn list_or_empty(table: &Table, key: &str) -> Vec<String> {
    match list(table, key) {
        Some(values) => values.to_vec(),
        None => Vec::new(),
    }
}

/// Refuses a field written as a number, a boolean or an inline table where the
/// register defines a string.
fn text_fields_are_strings(table: &Table, row: &str) -> Result<(), CatalogError> {
    for field in [
        "name", "path", "file", "crate", "feature", "default", "reason",
    ] {
        if let Some(value) = raw(table, field) {
            let refusal = CatalogError::BadValue {
                row: row.to_owned(),
                field: "value",
                value: format!("{field} = {value}"),
            };
            lgwks_std::trace::debug!(error = %refusal, "catalog: a row field is not a string");
            return Err(refusal);
        }
    }
    Ok(())
}

/// Reads one `[[capability]]` table.
fn read_row(table: &Table, index: usize) -> Result<Capability, CatalogError> {
    for entry in &table.entries {
        if !matches!(
            entry.key.as_str(),
            "name"
                | "path"
                | "file"
                | "crate"
                | "feature"
                | "default"
                | "reason"
                | "words"
                | "replaces"
        ) {
            let refusal = CatalogError::UnknownField {
                row: format!("row {}", index.saturating_add(1)),
                field: entry.key.clone(),
            };
            lgwks_std::trace::debug!(error = %refusal, "catalog: refusing an unknown row field");
            return Err(refusal);
        }
    }
    let unnamed = format!("row {}", index.saturating_add(1));
    text_fields_are_strings(table, &unnamed)?;
    let name = required(table, "name", &unnamed)?;
    let file = required(table, "file", &name)?;
    let krate = required(table, "crate", &name)?;
    let feature = required(table, "feature", &name)?;
    let default = required(table, "default", &name)?;
    if !CRATES.contains(&krate.as_str()) {
        let refusal = CatalogError::BadValue {
            row: name,
            field: "crate",
            value: krate,
        };
        lgwks_std::trace::debug!(error = %refusal, "catalog: a row names an unknown crate");
        return Err(refusal);
    }
    let default_on = match default.as_str() {
        "on" => true,
        "off" => false,
        other => {
            let refusal = CatalogError::BadValue {
                row: name,
                field: "default",
                value: other.to_owned(),
            };
            lgwks_std::trace::debug!(error = %refusal, "catalog: a row's default is not on or off");
            return Err(refusal);
        }
    };
    let reason = text(table, "reason").map(str::to_owned);
    if !default_on && reason.is_none() {
        let refusal = CatalogError::Missing {
            row: name,
            field: "reason",
        };
        lgwks_std::trace::debug!(error = %refusal, "catalog: a default-off row states no reason");
        return Err(refusal);
    }
    Ok(Capability {
        name,
        path: text(table, "path").map(str::to_owned),
        file,
        krate,
        feature,
        default_on,
        reason,
        words: list_or_empty(table, "words"),
        replaces: list_or_empty(table, "replaces"),
    })
}

/// Reads `root/CATALOG_PATH`.
pub fn read(root: &Path) -> Result<Catalog, CatalogError> {
    let path = root.join(CATALOG_PATH);
    let text = fs::read_to_string(&path).map_err(|cause| CatalogError::Io {
        path: path.clone(),
        cause,
    })?;
    let mut rows = Vec::new();
    for table in read_tables(&text)
        .iter()
        .filter(|table| table.path == "capability" && table.array)
    {
        rows.push(read_row(table, rows.len())?);
    }
    if rows.is_empty() {
        let refusal = CatalogError::Empty;
        lgwks_std::trace::debug!(error = %refusal, "catalog: the register declares no capability");
        return Err(refusal);
    }
    Ok(Catalog { rows })
}

/// `text` with the characters a Markdown table cell or a JSON string cannot
/// carry raw replaced.
fn escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '|' => out.push_str("\\|"),
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push(' '),
            other => out.push(other),
        }
    }
    out
}

/// One rendered line of the page.
fn line(out: &mut String, arguments: fmt::Arguments<'_>) -> Result<(), CatalogError> {
    use std::fmt::Write as _;
    writeln!(out, "{arguments}").map_err(|cause| CatalogError::Render { cause })
}

/// One line per capability for a terminal, in register order.
pub fn render_table(printable: &Catalog) -> Result<String, CatalogError> {
    let mut out = String::new();
    for row in printable.rows() {
        let state = if row.default_on() { "on" } else { "off" };
        line(
            &mut out,
            format_args!("{state:3}  {:<24} {}", row.name(), row.owner()),
        )?;
    }
    Ok(out)
}

/// The capability table as the published page renders it: one `<tr>` per
/// capability, owner linked to the repository path, and the default column
/// carrying the reason an `off` row is off.
pub fn render_site_section(catalog: &Catalog) -> Result<String, CatalogError> {
    let mut out = String::new();
    for row in catalog.rows() {
        let state = match (row.default_on(), row.reason()) {
            (true, _) => "on".to_owned(),
            (false, Some(reason)) => format!("off — {}", escaped(reason)),
            (false, None) => "off".to_owned(),
        };
        line(
            &mut out,
            format_args!(
                "<tr><td><code>{}</code></td><td><code>{}</code></td><td>{}</td><td>{}</td></tr>",
                escaped(row.name()),
                escaped(row.owner()),
                escaped(row.krate()),
                state
            ),
        )?;
    }
    Ok(out)
}

/// The generated page.
pub(crate) fn render_document(catalog: &Catalog) -> Result<String, CatalogError> {
    let mut out = String::new();
    out.push_str("# Capability catalog\n\n");
    out.push_str(
        "> Generated from [`contract/CATALOG.toml`](../contract/CATALOG.toml) by\n\
         > `lgwks-deps catalog --write`. Do not edit this page: change the register and\n\
         > re-render. The `catalog` lane refuses a page that differs from a fresh render.\n\n",
    );
    out.push_str(
        "Every capability this repository owns, the path that owns it, and whether it\n\
         is on by default. A row whose **Default** is `on` needs no feature flag: adding\n\
         the crate is the decision. An `off` row states why in ADR 0003.\n\n",
    );
    out.push_str("| Capability | Owner | Crate | Feature | Default | Search words |\n");
    out.push_str("|---|---|---|---|---|---|\n");
    for row in &catalog.rows {
        let default = match (row.default_on, row.reason.as_deref()) {
            (true, _) => "on".to_owned(),
            (false, Some(reason)) => format!("off — {}", escaped(reason)),
            (false, None) => "off".to_owned(),
        };
        let words: Vec<String> = row.words.iter().map(|word| escaped(word)).collect();
        line(
            &mut out,
            format_args!(
                "| {} | `{}` | `{}` | `{}` | {} | {} |",
                escaped(&row.name),
                escaped(row.owner()),
                escaped(&row.krate),
                escaped(&row.feature),
                default,
                words.join(", ")
            ),
        )?;
    }
    out.push_str(
        "\n## When the gate refuses your code\n\n\
         The std-first gate refuses a capability hand-rolled beside the module that\n\
         owns it, and names the owner. That name is a row above: read the owner's\n\
         module documentation, or print the machine-readable catalog with\n\
         `lgwks-deps catalog --json`. The refusal and this page are held together by\n\
         the `catalog` lane, which refuses an owner the gate can name and this page\n\
         does not list.\n",
    );
    Ok(out)
}

/// The catalog as one JSON document: `{"capabilities":[…]}`.
#[must_use]
pub fn render_json(catalog: &Catalog) -> String {
    let mut out = String::from("{\"capabilities\":[");
    for (index, row) in catalog.rows.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("{\"name\":\"");
        out.push_str(&escaped(&row.name));
        out.push_str("\",\"path\":");
        match row.path {
            Some(ref path) => {
                out.push('"');
                out.push_str(&escaped(path));
                out.push('"');
            }
            None => out.push_str("null"),
        }
        out.push_str(",\"file\":\"");
        out.push_str(&escaped(&row.file));
        out.push_str("\",\"crate\":\"");
        out.push_str(&escaped(&row.krate));
        out.push_str("\",\"feature\":\"");
        out.push_str(&escaped(&row.feature));
        out.push_str("\",\"default\":\"");
        out.push_str(if row.default_on { "on" } else { "off" });
        out.push_str("\",\"words\":[");
        for (position, word) in row.words.iter().enumerate() {
            if position > 0 {
                out.push(',');
            }
            out.push('"');
            out.push_str(&escaped(word));
            out.push('"');
        }
        out.push_str("],\"replaces\":[");
        for (position, value) in row.replaces.iter().enumerate() {
            if position > 0 {
                out.push(',');
            }
            out.push('"');
            out.push_str(&escaped(value));
            out.push('"');
        }
        out.push_str("]}");
    }
    out.push_str("]}\n");
    out
}

/// The features a manifest declares, from its `[features]` tables.
fn declared_features(root: &Path, krate: &str) -> Result<BTreeSet<String>, CatalogError> {
    if krate == "repository" {
        return Ok(BTreeSet::new());
    }
    let path = root
        .join("crates")
        .join(krate.replace('_', "-"))
        .join("Cargo.toml");
    let text = fs::read_to_string(&path).map_err(|cause| CatalogError::Io {
        path: path.clone(),
        cause,
    })?;
    let mut features = BTreeSet::new();
    for table in read_tables(&text) {
        if table.path == "features" {
            for entry in table.entries {
                features.insert(entry.key);
            }
        }
    }
    Ok(features)
}

/// Every `lgwks_*::path` spelled in `source`.
fn named_owners(source: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut offset = 0_usize;
    while let Some(found_at) = source.get(offset..).and_then(|rest| rest.find("lgwks_")) {
        let start = offset.saturating_add(found_at);
        let Some(tail) = source.get(start..) else {
            break;
        };
        let end = match tail
            .find(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == ':' || ch == '.'))
        {
            Some(value) => value,
            None => tail.len(),
        };
        if let Some(candidate) = tail.get(..end) {
            let trimmed = candidate.trim_end_matches([':', '.']);
            if trimmed.contains("::") && !found.iter().any(|known| known == trimmed) {
                found.push(trimmed.to_owned());
            }
        }
        offset = start.saturating_add(end.max(1));
    }
    found
}

/// Whether any row owns `named`: an exact path, or a module prefix of it.
fn is_owned(catalog: &Catalog, named: &str) -> bool {
    catalog.rows.iter().any(|row| match row.path {
        Some(ref path) => named == path || named.starts_with(&format!("{path}::")),
        None => false,
    })
}

/// Checks the catalog against the tree.
pub fn check(root: &Path, catalog: &Catalog) -> Result<Vec<String>, CatalogError> {
    let mut findings = Vec::new();
    let mut seen = BTreeSet::new();
    let mut features_by_crate: Vec<(String, BTreeSet<String>)> = Vec::new();
    for krate in CRATES {
        features_by_crate.push((krate.to_owned(), declared_features(root, krate)?));
    }
    for row in &catalog.rows {
        if !seen.insert(row.name.to_lowercase()) {
            findings.push(format!("{}: named twice", row.name));
        }
        if !root.join(&row.file).exists() {
            findings.push(format!("{}: {} does not exist", row.name, row.file));
        }
        if row.feature == "always" {
            continue;
        }
        let declared = features_by_crate
            .iter()
            .find(|pair| pair.0 == row.krate)
            .map(|pair| &pair.1);
        match declared {
            Some(declared) if declared.contains(&row.feature) => {}
            Some(_) => findings.push(format!(
                "{}: {} declares no feature `{}`",
                row.name, row.krate, row.feature
            )),
            None => findings.push(format!("{}: `{}` is not a crate", row.name, row.krate)),
        }
    }
    for source in GATE_SOURCES {
        let path = root.join(source);
        let text = fs::read_to_string(&path).map_err(|cause| CatalogError::Io {
            path: path.clone(),
            cause,
        })?;
        for named in named_owners(&text) {
            if !is_owned(catalog, &named) {
                findings.push(format!(
                    "{source} names `{named}`, which no catalog row owns: a refusal that \
                     points at an unlisted owner is the dead end the catalog exists to close"
                ));
            }
        }
    }
    Ok(findings)
}

/// Writes `root/DOC_PATH` from `catalog`.
pub fn write_document(root: &Path, catalog: &Catalog) -> Result<PathBuf, CatalogError> {
    let path = root.join(DOC_PATH);
    let rendered = render_document(catalog)?;
    fs::write(&path, rendered).map_err(|cause| CatalogError::Io {
        path: path.clone(),
        cause,
    })?;
    Ok(path)
}

/// Whether `root/DOC_PATH` is a fresh render of `catalog`.
pub fn document_is_current(root: &Path, catalog: &Catalog) -> Result<bool, CatalogError> {
    let path = root.join(DOC_PATH);
    match fs::read_to_string(&path) {
        Ok(committed) => Ok(committed == render_document(catalog)?),
        Err(cause) if cause.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(cause) => Err(CatalogError::Io { path, cause }),
    }
}

/// The catalog's own tests.
#[cfg(test)]
mod tests {
    use super::{
        Capability, Catalog, CatalogError, is_owned, named_owners, read, read_row, render_document,
        render_json, toml_subset,
    };
    use std::error::Error;
    use std::path::Path;

    /// A test's result type.
    type TestResult = Result<(), Box<dyn Error>>;

    /// One row, for the judgements below.
    fn row(name: &str, path: Option<&str>, default_on: bool) -> Capability {
        Capability {
            name: name.to_owned(),
            path: path.map(str::to_owned),
            file: "crates/lgwks-std/src/hash.rs".to_owned(),
            krate: "lgwks_std".to_owned(),
            feature: "hash".to_owned(),
            default_on,
            reason: if default_on {
                None
            } else {
                Some("measured".to_owned())
            },
            words: vec!["hash".to_owned()],
            replaces: vec!["a sha2 digest".to_owned()],
        }
    }

    /// The committed register parses, covers the whole estate, and names a file
    /// for every row.
    #[test]
    fn the_committed_register_parses_and_names_a_file() -> TestResult {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let catalog = read(&root)?;
        assert!(
            catalog.rows.len() >= 30,
            "the catalog must cover the whole estate, found {} rows",
            catalog.rows.len()
        );
        for entry in &catalog.rows {
            assert!(
                !entry.name.is_empty() && !entry.file.is_empty(),
                "a row must name a capability and its file"
            );
        }
        Ok(())
    }

    /// A refusal that names `lgwks_std::hash::Hasher` is owned by the row for
    /// `lgwks_std::hash`, and a named owner with no row is the finding.
    #[test]
    fn a_named_owner_resolves_to_its_module_row() {
        let catalog = Catalog {
            rows: vec![row("hash", Some("lgwks_std::hash"), true)],
        };
        assert!(is_owned(&catalog, "lgwks_std::hash"), "the module itself");
        assert!(
            is_owned(&catalog, "lgwks_std::hash::Hasher"),
            "a path inside the module"
        );
        assert!(
            !is_owned(&catalog, "lgwks_std::hex::encode"),
            "a named owner with no row is the gate defect"
        );
    }

    /// Only spelled-out paths are read out of a gate source: a crate name alone
    /// is not an owner name.
    #[test]
    fn only_a_qualified_path_is_read_out_of_a_gate_source() {
        let source = concat!(
            "`lgwks_std::hex::encode` is the encoder; see `lgwks_std` for the surface. ",
            "`lgwks_ast::scan`. "
        );
        assert_eq!(
            named_owners(source),
            vec![
                "lgwks_std::hex::encode".to_owned(),
                "lgwks_ast::scan".to_owned()
            ]
        );
    }

    /// A row with no `reason` and a default of `off` is refused: an opt-out
    /// without its number is the people-pleasing default the register forbids.
    #[test]
    fn an_off_row_without_a_reason_is_refused() -> TestResult {
        let text = "[[capability]]\nname = \"x\"\nfile = \"f\"\ncrate = \"lgwks_std\"\n\
                    feature = \"core\"\ndefault = \"off\"\n";
        let tables = toml_subset::read_tables(text);
        let Some(table) = tables.get(1) else {
            return Err("the fixture must declare one capability table".into());
        };
        match read_row(table, 0) {
            Err(CatalogError::Missing { field, .. }) => {
                assert_eq!(field, "reason", "the refusal names the missing field");
                Ok(())
            }
            other => Err(format!("expected a Missing refusal, got {other:?}").into()),
        }
    }

    /// The page and the JSON name every row, and the page says a default of
    /// `off` was measured rather than omitted.
    #[test]
    fn the_page_and_the_json_carry_every_row_and_its_default() -> TestResult {
        let catalog = Catalog {
            rows: vec![
                row("hash", Some("lgwks_std::hash"), true),
                row("gpui", Some("lgwks_deps::storefront"), false),
            ],
        };
        let page = render_document(&catalog)?;
        assert!(page.contains("| hash | `lgwks_std::hash` | `lgwks_std` | `hash` | on |"));
        assert!(page.contains("off — measured"), "an off row states why");
        assert!(page.contains("Do not edit this page"));
        let json = render_json(&catalog);
        assert!(json.starts_with("{\"capabilities\":["));
        assert!(json.trim_end().ends_with("]}"));
        assert_eq!(json.matches("\"name\":").count(), 2);
        assert!(json.contains("\"default\":\"off\""));
        Ok(())
    }

    /// A capability with no Rust path renders its file, so a documentation row
    /// still names its owner.
    #[test]
    fn a_documentation_row_renders_its_file() -> TestResult {
        let mut entry = row("documentation governance", None, true);
        entry.file = "docs/adr/README.md".to_owned();
        entry.krate = "repository".to_owned();
        entry.feature = "always".to_owned();
        let catalog = Catalog { rows: vec![entry] };
        assert!(render_document(&catalog)?.contains("| `docs/adr/README.md` |"));
        assert!(render_json(&catalog).contains("\"path\":null"));
        Ok(())
    }
}
