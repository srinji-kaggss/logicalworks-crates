//! Seeded sweeps over `parse` (INV-DEP-7, INV-DEP-10). Each test draws its own names, kinds,
//! sources and orders, writes a Cargo metadata document as text, and checks what the gate reads
//! against what the document says, never against the gate's own output.

use std::error::Error;

use lgwks_std::seeded::Seeded;

use super::{DependencyKind, DirectEdge, MetadataError, parse};

/// What every test here returns.
type TestResult = Result<(), Box<dyn Error>>;

/// The seeds each sweep covers.
const SEEDS: u64 = 400;

/// The registry source Cargo reports for crates.io.
const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";

/// A name unique inside one document: `index` keeps two draws apart, the word keeps seeds apart.
fn ident(stream: &mut Seeded, index: usize) -> Result<String, Box<dyn Error>> {
    Ok(format!("p{index}_{:x}", stream.below(1 << 32)?))
}

/// A JSON string literal; Rust's debug quoting of a plain ASCII word is valid JSON.
fn quoted(text: &str) -> String {
    format!("{text:?}")
}

/// A JSON string or `null`.
fn nullable(value: Option<&str>) -> String {
    match value {
        Some(text) => quoted(text),
        None => "null".to_owned(),
    }
}

/// One authored dependency declaration, as Cargo reports it.
struct Dep {
    /// Upstream package name.
    name: String,
    /// Cargo's source string, absent for a filesystem dependency.
    source: Option<String>,
    /// Manifest-relative path for a filesystem dependency.
    path: Option<String>,
    /// Semver requirement, verbatim.
    req: String,
    /// `normal`, `build`, `dev`, or absent.
    kind: Option<String>,
    /// Whether the manifest marks the edge optional.
    optional: bool,
    /// Enabled features, or `None` to omit the key.
    features: Option<Vec<String>>,
    /// `uses_default_features`, or `None` to omit the key.
    default_features: Option<bool>,
    /// Target `cfg(…)`, when scoped.
    target: Option<String>,
    /// Local alias, when renamed.
    rename: Option<String>,
}

impl Dep {
    /// A registry dependency with no optional keys.
    fn registry(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            source: Some(CRATES_IO.to_owned()),
            path: None,
            req: "^1".to_owned(),
            kind: None,
            optional: false,
            features: None,
            default_features: None,
            target: None,
            rename: None,
        }
    }

    /// A filesystem dependency with no `source`.
    fn path(name: &str, path: &str) -> Self {
        Self {
            source: None,
            path: Some(path.to_owned()),
            ..Self::registry(name)
        }
    }

    /// The declaration as the JSON object Cargo would emit.
    fn json(&self) -> String {
        let features = match self.features {
            Some(ref listed) => {
                let items: Vec<String> = listed.iter().map(|feature| quoted(feature)).collect();
                format!(",\"features\":[{}]", items.join(","))
            }
            None => String::new(),
        };
        let defaults = match self.default_features {
            Some(flag) => format!(",\"uses_default_features\":{flag}"),
            None => String::new(),
        };
        format!(
            "{{\"name\":{},\"source\":{},\"req\":{},\"kind\":{},\"optional\":{},\"path\":{}{features}{defaults},\"target\":{},\"rename\":{}}}",
            quoted(&self.name),
            nullable(self.source.as_deref()),
            quoted(&self.req),
            nullable(self.kind.as_deref()),
            self.optional,
            nullable(self.path.as_deref()),
            nullable(self.target.as_deref()),
            nullable(self.rename.as_deref()),
        )
    }
}

/// One package record.
struct Pkg {
    /// Opaque package id.
    id: String,
    /// Package name.
    name: String,
    /// Absolute manifest path, or `None` to write a `null`.
    manifest: Option<String>,
    /// Declared repository URL.
    repository: Option<String>,
    /// Declared SPDX expression.
    license: Option<String>,
    /// The dependencies this package authors.
    deps: Vec<Dep>,
}

impl Pkg {
    /// A workspace member living in `/w/<name>`.
    fn member(name: &str) -> Self {
        Self {
            id: format!("path+file:///w/{name}#{name}@0.1.0"),
            name: name.to_owned(),
            manifest: Some(format!("/w/{name}/Cargo.toml")),
            repository: None,
            license: None,
            deps: Vec::new(),
        }
    }

    /// A resolved package outside the workspace.
    fn external(name: &str) -> Self {
        Self {
            id: format!("registry+x#{name}@1.0.0"),
            manifest: None,
            ..Self::member(name)
        }
    }

    /// The record as the JSON object Cargo would emit.
    fn json(&self) -> String {
        let deps: Vec<String> = self.deps.iter().map(Dep::json).collect();
        format!(
            "{{\"id\":{},\"name\":{},\"repository\":{},\"manifest_path\":{},\"license\":{},\"license_file\":null,\"dependencies\":[{}]}}",
            quoted(&self.id),
            quoted(&self.name),
            nullable(self.repository.as_deref()),
            nullable(self.manifest.as_deref()),
            nullable(self.license.as_deref()),
            deps.join(",")
        )
    }
}

/// A whole response whose workspace members are `members`.
fn document(packages: &[Pkg], members: &[&str]) -> String {
    let records: Vec<String> = packages.iter().map(Pkg::json).collect();
    let listed: Vec<String> = members.iter().map(|id| quoted(id)).collect();
    format!(
        "{{\"packages\":[{}],\"workspace_members\":[{}]}}",
        records.join(","),
        listed.join(",")
    )
}

/// The edges a document reads as, with a refusal rendered for the test message.
fn edges(text: &str) -> Result<Vec<DirectEdge>, Box<dyn Error>> {
    parse(text).map_err(|error| format!("{error:?}").into())
}

/// The schema refusal a document ends in; any other outcome is a failure.
fn schema_refusal(text: &str) -> Result<String, Box<dyn Error>> {
    match parse(text) {
        Ok(read) => Err(format!("accepted with {} edges", read.len()).into()),
        Err(MetadataError::Schema(reason)) => Ok(reason),
        Err(other) => Err(format!("not a schema refusal: {other:?}").into()),
    }
}

/// A one-member document whose member declares `deps`.
fn lone(member: &str, deps: Vec<Dep>) -> String {
    let mut package = Pkg::member(member);
    let id = package.id.clone();
    package.deps = deps;
    document(&[package], &[&id])
}

/// The single edge a one-dependency document reads as.
fn single_edge(text: &str) -> Result<DirectEdge, Box<dyn Error>> {
    let mut read = edges(text)?;
    match (read.pop(), read.is_empty()) {
        (Some(edge), true) => Ok(edge),
        _ => Err("expected exactly one edge".into()),
    }
}

/// A two-member document: `a` declares one path dependency named `dep_name` at `path`, and `b`
/// declares a repository.
fn two_members(dep_name: &str, path: &str) -> String {
    let mut declaring = Pkg::member("a");
    declaring.deps = vec![Dep::path(dep_name, path)];
    let mut target = Pkg::member("b");
    target.repository = Some("https://example.test/b".to_owned());
    document(
        &[declaring, target],
        &["path+file:///w/a#a@0.1.0", "path+file:///w/b#b@0.1.0"],
    )
}

#[test]
fn sim_parse_orders_edges_by_package_whatever_the_input_order() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let count = usize::try_from(stream.below(6)?)?.saturating_add(2);
        let mut names = Vec::new();
        for index in 0..count {
            names.push(ident(&mut stream, index)?);
        }
        let mut shuffled = names.clone();
        for at in (1..shuffled.len()).rev() {
            let with = stream.index(at.saturating_add(1))?;
            shuffled.swap(at, with);
        }
        let read = edges(&lone(
            "m",
            shuffled.iter().map(|name| Dep::registry(name)).collect(),
        ))?;
        let mut expected = names;
        expected.sort();
        let got: Vec<&str> = read.iter().map(DirectEdge::package).collect();
        assert_eq!(
            got, expected,
            "seed {seed}: the edge order followed the input order"
        );
    }
    Ok(())
}

#[test]
fn sim_parse_reads_one_document_to_one_answer() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let first = ident(&mut stream, 0)?;
        let second = ident(&mut stream, 1)?;
        let text = lone("m", vec![Dep::registry(&first), Dep::registry(&second)]);
        assert_eq!(
            edges(&text)?,
            edges(&text)?,
            "seed {seed}: one document read two ways"
        );
    }
    Ok(())
}

#[test]
fn sim_parse_reads_an_absent_kind_as_normal_and_the_three_spellings_as_themselves() -> TestResult {
    let spellings = [
        (None, DependencyKind::Normal),
        (Some("normal"), DependencyKind::Normal),
        (Some("build"), DependencyKind::Build),
        (Some("dev"), DependencyKind::Dev),
    ];
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let (spelling, kind) = spellings[stream.index(spellings.len())?];
        let mut dep = Dep::registry(&ident(&mut stream, 0)?);
        dep.kind = spelling.map(str::to_owned);
        assert_eq!(
            single_edge(&lone("m", vec![dep]))?.kind,
            kind,
            "seed {seed}: kind {spelling:?} read wrongly"
        );
    }
    Ok(())
}

#[test]
fn sim_parse_refuses_a_kind_it_does_not_know() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let mut dep = Dep::registry(&ident(&mut stream, 0)?);
        dep.kind = Some(format!("k{:x}", stream.below(1 << 20)?));
        let reason = schema_refusal(&lone("m", vec![dep]))?;
        assert!(
            reason.contains("unknown Cargo dependency kind"),
            "seed {seed}: refused for the wrong reason: {reason}"
        );
    }
    Ok(())
}

#[test]
fn sim_parse_classifies_every_source_scheme() -> TestResult {
    let schemes = [
        ("registry+https://r.example/index", "registry"),
        ("sparse+https://r.example/", "registry"),
        ("git+https://g.example/repo#abc", "git"),
        ("ssh+https://o.example/x", "other"),
    ];
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let (scheme, class) = schemes[stream.index(schemes.len())?];
        let mut dep = Dep::registry(&ident(&mut stream, 0)?);
        dep.source = Some(scheme.to_owned());
        let edge = single_edge(&lone("m", vec![dep]))?;
        assert_eq!(
            edge.source.class(),
            class,
            "seed {seed}: {scheme} misclassified"
        );
        assert_eq!(
            edge.source.detail(),
            scheme,
            "seed {seed}: the source was not kept verbatim"
        );
    }
    Ok(())
}

#[test]
fn sim_parse_prefers_the_source_key_over_the_path_key() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let mut dep = Dep::path(&ident(&mut stream, 0)?, "../elsewhere");
        dep.source = Some(CRATES_IO.to_owned());
        let edge = single_edge(&lone("m", vec![dep]))?;
        assert_eq!(
            edge.source.class(),
            "registry",
            "seed {seed}: the path beat the source"
        );
        assert!(
            !edge.workspace,
            "seed {seed}: a sourced edge is never a member"
        );
    }
    Ok(())
}

#[test]
fn sim_parse_refuses_an_edge_with_neither_source_nor_path() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let mut dep = Dep::registry(&ident(&mut stream, 0)?);
        dep.source = None;
        let reason = schema_refusal(&lone("m", vec![dep]))?;
        assert!(
            reason.contains("neither source nor path"),
            "seed {seed}: refused for the wrong reason: {reason}"
        );
    }
    Ok(())
}

#[test]
fn sim_parse_reads_an_omitted_default_features_key_as_true() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let flag = stream.below(2)? == 0;
        let mut omitted = Dep::registry(&ident(&mut stream, 0)?);
        omitted.default_features = None;
        let mut stated = Dep::registry(&ident(&mut stream, 1)?);
        stated.default_features = Some(flag);
        assert!(
            single_edge(&lone("m", vec![omitted]))?.uses_default_features(),
            "seed {seed}: an omitted key must read as the Cargo default"
        );
        assert_eq!(
            single_edge(&lone("m", vec![stated]))?.uses_default_features(),
            flag,
            "seed {seed}: a stated key was not kept"
        );
    }
    Ok(())
}

#[test]
fn sim_parse_keeps_features_in_the_order_cargo_emitted_them() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let count = usize::try_from(stream.below(6)?)?;
        let mut listed = Vec::new();
        for index in 0..count {
            listed.push(ident(&mut stream, index)?);
        }
        let mut dep = Dep::registry(&ident(&mut stream, 99)?);
        dep.features = Some(listed.clone());
        assert_eq!(
            single_edge(&lone("m", vec![dep]))?.features(),
            listed.as_slice(),
            "seed {seed}: the feature list was reordered or changed"
        );
        let bare = Dep::registry(&ident(&mut stream, 98)?);
        assert!(
            single_edge(&lone("m", vec![bare]))?.features().is_empty(),
            "seed {seed}: an omitted list must read empty"
        );
    }
    Ok(())
}

#[test]
fn sim_parse_keeps_the_optional_flag() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let flag = stream.below(2)? == 0;
        let mut dep = Dep::registry(&ident(&mut stream, 0)?);
        dep.optional = flag;
        assert_eq!(
            single_edge(&lone("m", vec![dep]))?.optional,
            flag,
            "seed {seed}: the optional flag changed"
        );
    }
    Ok(())
}

#[test]
fn sim_parse_keeps_the_target_and_the_rename_apart_from_the_package() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let package = ident(&mut stream, 0)?;
        let alias = ident(&mut stream, 1)?;
        let mut dep = Dep::registry(&package);
        dep.target = Some("cfg(unix)".to_owned());
        dep.rename = Some(alias.clone());
        let edge = single_edge(&lone("m", vec![dep]))?;
        assert_eq!(
            edge.package(),
            package,
            "seed {seed}: the package identity moved"
        );
        assert_eq!(
            edge.rename(),
            Some(alias.as_str()),
            "seed {seed}: the alias was lost"
        );
        assert_eq!(
            edge.target(),
            Some("cfg(unix)"),
            "seed {seed}: the target was lost"
        );
    }
    Ok(())
}

#[test]
fn sim_parse_keeps_the_requirement_byte_for_byte() -> TestResult {
    let shapes = ["^1", "1", "~1.2", ">=1, <3", "=1.2.3", "*"];
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let shape = shapes[stream.index(shapes.len())?];
        let mut dep = Dep::registry(&ident(&mut stream, 0)?);
        dep.req = shape.to_owned();
        assert_eq!(
            single_edge(&lone("m", vec![dep]))?.requirement(),
            shape,
            "seed {seed}: the requirement was normalised"
        );
    }
    Ok(())
}

#[test]
fn sim_parse_marks_a_path_that_reaches_a_member_directory_as_a_member() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let detours = usize::try_from(stream.below(3)?)?;
        let mut path = "..".to_owned();
        for _ in 0..detours {
            path = format!("{path}/x/..");
        }
        let edge = single_edge(&two_members("b", &format!("{path}/b")))?;
        assert!(
            edge.workspace,
            "seed {seed}: {path}/b reaches /w/b and must read as a member"
        );
        assert_eq!(
            edge.target_repository.as_deref(),
            Some("https://example.test/b"),
            "seed {seed}: the member's repository was not carried"
        );
    }
    Ok(())
}

#[test]
fn sim_parse_does_not_take_an_outside_path_with_a_members_name_for_a_member() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let outside = ident(&mut stream, 0)?;
        let edge = single_edge(&two_members("b", &format!("../../{outside}/b")))?;
        assert!(
            !edge.workspace,
            "seed {seed}: a lookalike outside the workspace joined it"
        );
        assert_eq!(
            edge.target_repository, None,
            "seed {seed}: a lookalike borrowed a repository"
        );
    }
    Ok(())
}

#[test]
fn sim_parse_refuses_a_path_into_a_member_under_another_name() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let wrong = ident(&mut stream, 0)?;
        let reason = schema_refusal(&two_members(&wrong, "../b"))?;
        assert!(
            reason.contains("resolves to workspace package"),
            "seed {seed}: refused for the wrong reason: {reason}"
        );
    }
    Ok(())
}

#[test]
fn sim_parse_reads_a_path_that_escapes_the_root_as_external() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let ups = vec![".."; usize::try_from(stream.below(4)?)?.saturating_add(3)].join("/");
        let edge = single_edge(&two_members("b", &format!("{ups}/b")))?;
        assert!(
            !edge.workspace,
            "seed {seed}: an unlocatable path must fail closed to external"
        );
    }
    Ok(())
}

#[test]
fn sim_parse_refuses_a_duplicate_package_id() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let name = ident(&mut stream, 0)?;
        let one = Pkg::member(&name);
        let two = Pkg::member(&name);
        let id = one.id.clone();
        let reason = schema_refusal(&document(&[one, two], &[&id]))?;
        assert!(
            reason.contains("duplicate Cargo package id"),
            "seed {seed}: {reason}"
        );
    }
    Ok(())
}

#[test]
fn sim_parse_refuses_a_duplicate_workspace_member_id() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let package = Pkg::member(&ident(&mut stream, 0)?);
        let id = package.id.clone();
        let reason = schema_refusal(&document(&[package], &[&id, &id]))?;
        assert!(
            reason.contains("duplicate Cargo workspace member id"),
            "seed {seed}: {reason}"
        );
    }
    Ok(())
}

#[test]
fn sim_parse_refuses_a_member_id_with_no_package_record() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let present = Pkg::member(&ident(&mut stream, 0)?);
        let absent = Pkg::member(&ident(&mut stream, 1)?);
        let reason = schema_refusal(&document(&[present], &[&absent.id]))?;
        assert!(
            reason.contains("has no package record"),
            "seed {seed}: {reason}"
        );
    }
    Ok(())
}

#[test]
fn sim_parse_refuses_a_blank_package_name() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let mut package = Pkg::member(&ident(&mut stream, 0)?);
        package.name = " ".repeat(usize::try_from(stream.below(4)?)?);
        let id = package.id.clone();
        let reason = schema_refusal(&document(&[package], &[&id]))?;
        assert!(reason.contains("blank id or name"), "seed {seed}: {reason}");
    }
    Ok(())
}

#[test]
fn sim_parse_refuses_a_member_with_no_manifest_path() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let mut package = Pkg::member(&ident(&mut stream, 0)?);
        package.manifest = None;
        let id = package.id.clone();
        let reason = schema_refusal(&document(&[package], &[&id]))?;
        assert!(
            reason.contains("has no manifest_path"),
            "seed {seed}: {reason}"
        );
    }
    Ok(())
}

#[test]
fn sim_parse_refuses_two_members_that_claim_one_directory() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let first = Pkg::member(&ident(&mut stream, 0)?);
        let mut second = Pkg::member(&ident(&mut stream, 1)?);
        second.manifest = first.manifest.clone();
        let ids = [first.id.clone(), second.id.clone()];
        let reason = schema_refusal(&document(&[first, second], &[&ids[0], &ids[1]]))?;
        assert!(
            reason.contains("claim manifest directory"),
            "seed {seed}: {reason}"
        );
    }
    Ok(())
}

#[test]
fn sim_parse_carries_the_licence_of_the_package_cargo_resolved() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let target = ident(&mut stream, 0)?;
        let licence = format!("MIT OR L{:x}", stream.below(1 << 16)?);
        let mut resolved = Pkg::external(&target);
        resolved.license = Some(licence.clone());
        let mut member = Pkg::member("m");
        member.deps = vec![Dep::registry(&target), Dep::registry("unseen")];
        let id = member.id.clone();
        let read = edges(&document(&[member, resolved], &[&id]))?;
        let by_name = |name: &str| read.iter().find(|edge| edge.package() == name);
        assert_eq!(
            by_name(&target).and_then(DirectEdge::license),
            Some(licence.as_str()),
            "seed {seed}: the resolved licence did not travel"
        );
        assert_eq!(
            by_name("unseen").and_then(DirectEdge::license),
            None,
            "seed {seed}: an unresolved package must read as no licence, not an empty one"
        );
    }
    Ok(())
}

#[test]
fn sim_parse_binds_a_shared_name_to_the_first_record_in_the_document() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let target = ident(&mut stream, 0)?;
        let mut first = Pkg::external(&target);
        first.license = Some("MIT".to_owned());
        let mut second = Pkg::external(&target);
        second.id = format!("registry+x#{target}@2.0.0");
        second.license = Some("GPL-3.0".to_owned());
        let mut member = Pkg::member("m");
        member.deps = vec![Dep::registry(&target)];
        let id = member.id.clone();
        let edge = single_edge(&document(&[member, first, second], &[&id]))?;
        assert_eq!(
            edge.license(),
            Some("MIT"),
            "seed {seed}: a shared name took the later record's licence"
        );
    }
    Ok(())
}

#[test]
fn sim_parse_reports_malformed_text_as_a_json_refusal() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let cut = usize::try_from(stream.below(40)?)?;
        let whole = lone("m", vec![Dep::registry(&ident(&mut stream, 0)?)]);
        let broken = whole
            .get(..cut.min(whole.len().saturating_sub(1)))
            .ok_or("a prefix of an ASCII document")?;
        assert!(
            matches!(parse(broken), Err(MetadataError::Json(_))),
            "seed {seed}: a truncated document must be a JSON refusal"
        );
    }
    Ok(())
}
