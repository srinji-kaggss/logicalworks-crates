//! Deterministic seeded simulation of `lgwks_deps::metadata::parse` over
//! generated `cargo metadata --format-version 1` documents (issue #158 A2/A3).
//!
//! One `u64` seed drives the document: how many workspace members exist, which
//! authored dependency dimension each member varies (`kind`, `target`,
//! `optional`, `default-features`, `features`, `rename`, or a `path` edge), and
//! — in the fault families — which structural inconsistency Cargo's answer
//! carries (a duplicate package record, a missing record, a repeated member id,
//! a member omitted from the manifest path, a member/path name mismatch, two
//! members sharing one manifest directory, a blank identity, an unknown
//! dependency kind, a declaration with neither source nor path, truncated
//! bytes, or a wrongly typed field).
//!
//! Each scenario is modelled independently of the parser — the exact decoded
//! edge set, or the specific typed refusal — and `metadata::parse` must agree
//! with the model for every seed. A failure prints its seed. The generator is a
//! small splitmix64 written here, so there is no new dependency, no wall clock
//! and no OS entropy: the same seed always produces the same document and the
//! same trace.

use std::collections::hash_map::DefaultHasher;
use std::error::Error;
use std::hash::{Hash, Hasher};

use lgwks_deps::metadata::{self, DirectEdge, MetadataError};

/// What every test in this suite returns.
type TestResult = Result<(), Box<dyn Error>>;

/// The one registry origin the generated documents name.
const REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";

/// Every authored dependency dimension the general generator can vary.
const ALL_DIMS: &[Dim] = &[
    Dim::Kind,
    Dim::Target,
    Dim::Optional,
    Dim::DefaultFeatures,
    Dim::Features,
    Dim::Rename,
    Dim::Path,
];

// ── Deterministic generator ─────────────────────────────────────────────────

/// A splitmix64 generator: one seed fixes the whole sequence.
///
/// Written here rather than pulled in, so the suite adds no dependency and uses
/// no wall clock or OS entropy that a replay could not reproduce.
struct Rng(u64);

impl Rng {
    /// A generator whose entire sequence is determined by `seed`.
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// The next 64-bit value in the sequence.
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut mixed = self.0;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        mixed ^ (mixed >> 31)
    }

    /// A value in `0..bound`, or `0` when `bound` is zero.
    fn below(&mut self, bound: u64) -> u64 {
        self.next_u64().checked_rem(bound.max(1)).unwrap_or(0)
    }

    /// An index in `0..bound`.
    fn index(&mut self, bound: usize) -> usize {
        usize::try_from(self.below(u64::try_from(bound).unwrap_or(1))).unwrap_or(0)
    }

    /// A boolean drawn from the generator's high bit.
    fn coin(&mut self) -> bool {
        self.next_u64() >> 63 == 1
    }

    /// A reference into `values`, drawn from the generator.
    fn pick<'a, T>(&mut self, values: &'a [T]) -> &'a T {
        &values[self.index(values.len())]
    }
}

// ── The model ───────────────────────────────────────────────────────────────

/// One authored dependency dimension a member's declaration varies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dim {
    /// The `kind` (normal / build / dev).
    Kind,
    /// The `target` cfg scope.
    Target,
    /// The `optional` bit.
    Optional,
    /// The `default-features` bit.
    DefaultFeatures,
    /// The enabled `features` list.
    Features,
    /// The local `package =` rename.
    Rename,
    /// A `path` edge (internal member or external directory).
    Path,
}

/// A dependency edge as the model predicts it, in exactly the fields `parse`
/// returns through the public accessors.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ModeledEdge {
    /// Workspace package declaring the edge.
    consumer: String,
    /// Upstream package identity.
    package: String,
    /// Authored semver requirement.
    requirement: String,
    /// `normal`, `build`, or `dev`.
    kind: &'static str,
    /// `registry`, `git`, `path`, or `other`.
    class: &'static str,
    /// The exact Cargo source or path string.
    detail: String,
    /// Whether the declaration is optional.
    optional: bool,
    /// Whether the target is a workspace member.
    workspace: bool,
    /// Repository of a workspace path target, when it declares one.
    target_repository: Option<String>,
    /// Enabled features, in emitted order.
    features: Vec<String>,
    /// Whether default features are enabled.
    uses_default_features: bool,
    /// Target cfg scope.
    target: Option<String>,
    /// Local rename alias.
    rename: Option<String>,
}

/// The independently modelled outcome of one generated document.
#[derive(Debug)]
enum Model {
    /// The document decodes to exactly these edges.
    Edges(Vec<ModeledEdge>),
    /// The document is refused with a schema message containing this fragment.
    Schema(&'static str),
    /// The document is refused as invalid JSON.
    Json,
}

/// A generated document and the model's expectation for it.
struct Scenario {
    /// The bytes handed to `parse`.
    document: String,
    /// The expected outcome.
    model: Model,
}

/// What `parse` actually returned, normalized for comparison with the model.
#[derive(Debug)]
enum Observed {
    /// A decoded edge set, sorted.
    Edges(Vec<ModeledEdge>),
    /// A schema refusal carrying the message.
    Schema(String),
    /// A JSON decode refusal.
    Json,
    /// A refusal shape `parse` should never produce.
    Unexpected,
}

/// Projects one decoded edge into the model's comparable shape.
fn modeled(edge: &DirectEdge) -> ModeledEdge {
    ModeledEdge {
        consumer: edge.consumer().to_owned(),
        package: edge.package().to_owned(),
        requirement: edge.requirement().to_owned(),
        kind: edge.kind.as_str(),
        class: edge.source.class(),
        detail: edge.source.detail().to_owned(),
        optional: edge.optional,
        workspace: edge.workspace,
        target_repository: edge.target_repository.clone(),
        features: edge.features().to_vec(),
        uses_default_features: edge.uses_default_features(),
        target: edge.target().map(str::to_owned),
        rename: edge.rename().map(str::to_owned),
    }
}

/// Normalizes a `parse` result into the model's comparable shape.
fn observe(result: Result<Vec<DirectEdge>, MetadataError>) -> Observed {
    match result {
        Ok(edges) => {
            let mut projected: Vec<ModeledEdge> = edges.iter().map(modeled).collect();
            projected.sort();
            Observed::Edges(projected)
        }
        Err(MetadataError::Json(_)) => Observed::Json,
        Err(MetadataError::Schema(message)) => Observed::Schema(message),
        Err(_) => Observed::Unexpected,
    }
}

/// The policy class a Cargo `source` string names.
fn class_of(source: &str) -> &'static str {
    if source.starts_with("registry+") || source.starts_with("sparse+") {
        "registry"
    } else if source.starts_with("git+") {
        "git"
    } else {
        "other"
    }
}

/// The model's kind spelling for a Cargo `kind` value.
fn kind_of(kind: Option<&str>) -> &'static str {
    match kind {
        Some("build") => "build",
        Some("dev") => "dev",
        _ => "normal",
    }
}

// ── Document assembly ───────────────────────────────────────────────────────

/// One authored dependency declaration in the generated document.
#[derive(Clone)]
struct DepPlan {
    /// Upstream package name.
    name: String,
    /// Cargo `source` string, or `None` for a path edge.
    source: Option<String>,
    /// Manifest semver requirement.
    req: String,
    /// `kind` spelling, or `None` for Cargo's normal default.
    kind: Option<&'static str>,
    /// Whether the declaration is optional.
    optional: bool,
    /// Manifest path for a path edge.
    path: Option<String>,
    /// Enabled features.
    features: Vec<String>,
    /// Whether default features are enabled.
    uses_default_features: bool,
    /// Target cfg scope.
    target: Option<String>,
    /// Local rename alias.
    rename: Option<String>,
}

impl DepPlan {
    /// A registry declaration with Cargo's defaults.
    fn registry(name: String) -> Self {
        Self {
            name,
            source: Some(REGISTRY.to_owned()),
            req: "1.0".to_owned(),
            kind: None,
            optional: false,
            path: None,
            features: Vec::new(),
            uses_default_features: true,
            target: None,
            rename: None,
        }
    }
}

/// One workspace member in the generated document.
struct MemberPlan {
    /// Cargo package id.
    id: String,
    /// Cargo package name.
    name: String,
    /// Declared repository URL, when any.
    repository: Option<String>,
    /// Manifest path, or `None` to omit the key entirely.
    manifest_path: Option<String>,
}

/// Renders an optional JSON string value.
fn json_string(value: Option<&str>) -> String {
    match value {
        Some(text) => format!("\"{text}\""),
        None => "null".to_owned(),
    }
}

/// Renders one dependency declaration as Cargo JSON.
fn dep_json(dep: &DepPlan) -> String {
    let source = json_string(dep.source.as_deref());
    let kind = json_string(dep.kind);
    let path = json_string(dep.path.as_deref());
    let target = json_string(dep.target.as_deref());
    let rename = json_string(dep.rename.as_deref());
    let features = dep
        .features
        .iter()
        .map(|feature| format!("\"{feature}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"name\":\"{name}\",\"source\":{source},\"req\":\"{req}\",\"kind\":{kind},\"optional\":{optional},\"path\":{path},\"features\":[{features}],\"uses_default_features\":{defaults},\"target\":{target},\"rename\":{rename}}}",
        name = dep.name,
        source = source,
        req = dep.req,
        kind = kind,
        optional = dep.optional,
        path = path,
        features = features,
        defaults = dep.uses_default_features,
        target = target,
        rename = rename,
    )
}

/// Renders one package record, omitting `manifest_path` when the plan has none.
fn member_json(member: &MemberPlan, deps: &[DepPlan]) -> String {
    let repository = json_string(member.repository.as_deref());
    let dependencies = deps.iter().map(dep_json).collect::<Vec<_>>().join(",");
    let manifest = member
        .manifest_path
        .as_deref()
        .map_or_else(String::new, |path| format!(",\"manifest_path\":\"{path}\""));
    format!(
        "{{\"id\":\"{id}\",\"name\":\"{name}\",\"repository\":{repository},\"dependencies\":[{dependencies}]{manifest}}}",
        id = member.id,
        name = member.name,
        repository = repository,
        dependencies = dependencies,
        manifest = manifest,
    )
}

/// Assembles a metadata document from package records and member ids.
fn document(packages: &[String], members: &[String]) -> String {
    let joined_packages = packages.join(",");
    let joined_members = members
        .iter()
        .map(|identifier| format!("\"{identifier}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{\"packages\":[{joined_packages}],\"workspace_members\":[{joined_members}]}}")
}

// ── Scenarios ───────────────────────────────────────────────────────────────

/// A generated valid workspace: the records to serialize, the plans behind
/// them, the member ids, and the model edge each member decodes to.
struct Workspace {
    /// Serialized package records, one per member, in member order.
    records: Vec<String>,
    /// The member/declaration plans, for faults that re-serialize.
    plans: Vec<(MemberPlan, DepPlan)>,
    /// Member package ids, referenced by `workspace_members`.
    ids: Vec<String>,
    /// The modelled edge each member authors.
    edges: Vec<ModeledEdge>,
}

impl Workspace {
    /// The document bytes for the current records and ids.
    fn document(&self) -> String {
        document(&self.records, &self.ids)
    }

    /// Re-serializes the records after a fault edited a plan.
    fn reserialize(&mut self) {
        let records: Vec<String> = self
            .plans
            .iter()
            .map(|pair| member_json(&pair.0, std::slice::from_ref(&pair.1)))
            .collect();
        self.records = records;
    }
}

/// Builds one member varying `dim`, with the model edge it decodes to.
fn build_member(
    index: usize,
    dim: Dim,
    names: &[String],
    repositories: &[Option<String>],
    rng: &mut Rng,
) -> (MemberPlan, DepPlan, ModeledEdge) {
    let consumer = names[index].clone();
    let member = MemberPlan {
        id: format!("path+file:///repo/{consumer}#{consumer}@0.1.0"),
        name: consumer.clone(),
        repository: repositories[index].clone(),
        manifest_path: Some(format!("/repo/{consumer}/Cargo.toml")),
    };
    let mut dep = DepPlan::registry(format!("dep{index}"));
    match dim {
        Dim::Kind => dep.kind = Some(*rng.pick(&["normal", "build", "dev"])),
        Dim::Target => {
            if rng.coin() {
                dep.target = Some("cfg(unix)".to_owned());
            }
        }
        Dim::Optional => dep.optional = rng.coin(),
        Dim::DefaultFeatures => dep.uses_default_features = rng.coin(),
        Dim::Features => {
            dep.features = ["a", "b", "c"]
                .iter()
                .copied()
                .filter(|_| rng.coin())
                .map(str::to_owned)
                .collect();
        }
        Dim::Rename => {
            if rng.coin() {
                dep.rename = Some(format!("alias{index}"));
            }
        }
        Dim::Path => {
            dep.source = None;
            if rng.coin() {
                let target = rng.index(names.len());
                dep.name.clone_from(&names[target]);
                dep.path = Some(format!("../{}", names[target]));
            } else {
                dep.path = Some(format!("../outside/dep{index}"));
            }
        }
    }
    let (class, detail) = match (dep.source.as_deref(), dep.path.as_deref()) {
        (Some(source), _) => (class_of(source), source.to_owned()),
        (None, Some(path)) => ("path", path.to_owned()),
        (None, None) => ("other", String::new()),
    };
    let mut edge = ModeledEdge {
        consumer,
        package: dep.name.clone(),
        requirement: dep.req.clone(),
        kind: kind_of(dep.kind),
        class,
        detail,
        optional: dep.optional,
        workspace: false,
        target_repository: None,
        features: dep.features.clone(),
        uses_default_features: dep.uses_default_features,
        target: dep.target.clone(),
        rename: dep.rename.clone(),
    };
    // The generator's only path shapes are `../<member>` and
    // `../outside/<dep>`, so membership is decided by the leading component.
    if dep.source.is_none()
        && let Some(relative) = dep
            .path
            .as_deref()
            .and_then(|path| path.strip_prefix("../"))
        && let Some(position) = names
            .iter()
            .position(|candidate| candidate.as_str() == relative)
    {
        edge.workspace = true;
        edge.target_repository.clone_from(&repositories[position]);
    }
    (member, dep, edge)
}

/// Builds a valid workspace varying `allowed` dimensions, one per member.
fn workspace(seed: u64, allowed: &[Dim]) -> Workspace {
    let mut rng = Rng::new(seed);
    let count = usize::try_from(rng.below(6).saturating_add(1)).unwrap_or(1);
    let mut names = Vec::new();
    let mut repositories = Vec::new();
    for slot in 0..count {
        let name = format!("m{slot}");
        let repository = if rng.coin() {
            Some(format!("https://example.invalid/{name}"))
        } else {
            None
        };
        names.push(name);
        repositories.push(repository);
    }
    let mut plans = Vec::new();
    let mut records = Vec::new();
    let mut ids = Vec::new();
    let mut edges = Vec::new();
    for index in 0..count {
        let dim = if allowed.is_empty() {
            Dim::Kind
        } else {
            *rng.pick(allowed)
        };
        let (member, dep, edge) = build_member(index, dim, &names, &repositories, &mut rng);
        ids.push(member.id.clone());
        records.push(member_json(&member, std::slice::from_ref(&dep)));
        plans.push((member, dep));
        edges.push(edge);
    }
    Workspace {
        records,
        plans,
        ids,
        edges,
    }
}

/// A valid scenario varying `allowed` dimensions.
fn valid_scenario(seed: u64, allowed: &[Dim]) -> Scenario {
    let built = workspace(seed, allowed);
    let document = built.document();
    Scenario {
        document,
        model: Model::Edges(built.edges),
    }
}

/// A duplicate package record for the first member is a schema refusal.
fn duplicate_package_scenario(seed: u64) -> Scenario {
    let mut built = workspace(seed, ALL_DIMS);
    if let Some(first) = built.records.first().cloned() {
        built.records.push(first);
    }
    Scenario {
        document: built.document(),
        model: Model::Schema("duplicate Cargo package id"),
    }
}

/// A workspace member id with no package record is a schema refusal.
fn missing_package_scenario(seed: u64) -> Scenario {
    let mut built = workspace(seed, ALL_DIMS);
    built
        .ids
        .push("path+file:///repo/ghost#ghost@0.1.0".to_owned());
    Scenario {
        document: built.document(),
        model: Model::Schema("has no package record"),
    }
}

/// A repeated workspace member id is a schema refusal.
fn duplicate_member_scenario(seed: u64) -> Scenario {
    let mut built = workspace(seed, ALL_DIMS);
    if let Some(first) = built.ids.first().cloned() {
        built.ids.push(first);
    }
    Scenario {
        document: built.document(),
        model: Model::Schema("duplicate Cargo workspace member id"),
    }
}

/// A workspace member with no `manifest_path` is a schema refusal.
fn missing_manifest_scenario(seed: u64) -> Scenario {
    let mut built = workspace(seed, ALL_DIMS);
    if let Some(pair) = built.plans.first_mut() {
        pair.0.manifest_path = None;
    }
    built.reserialize();
    Scenario {
        document: built.document(),
        model: Model::Schema("has no manifest_path"),
    }
}

/// A blank package id or name is a schema refusal.
fn blank_identity_scenario(seed: u64) -> Scenario {
    let mut rng = Rng::new(seed);
    let mut built = workspace(seed, ALL_DIMS);
    let blank_id = rng.coin();
    if let Some(pair) = built.plans.first_mut() {
        if blank_id {
            pair.0.id.clear();
        } else {
            pair.0.name.clear();
        }
    }
    built.reserialize();
    Scenario {
        document: built.document(),
        model: Model::Schema("blank id or name"),
    }
}

/// An unknown Cargo dependency kind is a schema refusal, never a default.
fn unknown_kind_scenario(seed: u64) -> Scenario {
    let mut built = workspace(seed, ALL_DIMS);
    if let Some(pair) = built.plans.first_mut() {
        pair.1.source = Some(REGISTRY.to_owned());
        pair.1.path = None;
        pair.1.kind = Some("proc-macro");
    }
    built.reserialize();
    Scenario {
        document: built.document(),
        model: Model::Schema("unknown Cargo dependency kind"),
    }
}

/// A declaration with neither source nor path is a schema refusal.
fn unsourced_scenario(seed: u64) -> Scenario {
    let mut built = workspace(seed, ALL_DIMS);
    if let Some(pair) = built.plans.first_mut() {
        pair.1.source = None;
        pair.1.path = None;
        pair.1.kind = None;
    }
    built.reserialize();
    Scenario {
        document: built.document(),
        model: Model::Schema("has neither source nor path"),
    }
}

/// A path edge that resolves into a member directory under another name is a
/// schema refusal, never a quiet misattribution.
fn mismatch_scenario(seed: u64) -> Scenario {
    let mut rng = Rng::new(seed);
    let first = MemberPlan {
        id: "app".to_owned(),
        name: "app".to_owned(),
        repository: None,
        manifest_path: Some("/repo/app/Cargo.toml".to_owned()),
    };
    let second = MemberPlan {
        id: "helper".to_owned(),
        name: "helper".to_owned(),
        repository: None,
        manifest_path: Some("/repo/helper/Cargo.toml".to_owned()),
    };
    let wrong_name = *rng.pick(&["renamed", "helper_copy"]);
    let dep = DepPlan {
        name: wrong_name.to_owned(),
        source: None,
        req: "*".to_owned(),
        kind: None,
        optional: false,
        path: Some("../helper".to_owned()),
        features: Vec::new(),
        uses_default_features: true,
        target: None,
        rename: None,
    };
    let records = vec![
        member_json(&first, std::slice::from_ref(&dep)),
        member_json(&second, &[]),
    ];
    let ids = vec![first.id, second.id];
    Scenario {
        document: document(&records, &ids),
        model: Model::Schema("resolves to workspace package"),
    }
}

/// Two members claiming one manifest directory are refused, not merged.
fn shared_dir_scenario(seed: u64) -> Scenario {
    let mut rng = Rng::new(seed);
    let shared = *rng.pick(&["/repo/shared", "/repo/common", "/repo/dup"]);
    let first = MemberPlan {
        id: "first".to_owned(),
        name: "first".to_owned(),
        repository: None,
        manifest_path: Some(format!("{shared}/Cargo.toml")),
    };
    let second = MemberPlan {
        id: "second".to_owned(),
        name: "second".to_owned(),
        repository: None,
        manifest_path: Some(format!("{shared}/Cargo.toml")),
    };
    let records = vec![member_json(&first, &[]), member_json(&second, &[])];
    let ids = vec![first.id, second.id];
    Scenario {
        document: document(&records, &ids),
        model: Model::Schema("multiple workspace packages claim manifest directory"),
    }
}

/// A strict prefix of a valid document is invalid JSON, never a partial graph.
fn truncated_scenario(seed: u64) -> Scenario {
    let mut rng = Rng::new(seed);
    let full = workspace(seed, ALL_DIMS).document();
    let length = full.len();
    let span = u64::try_from(length.saturating_sub(1)).unwrap_or(1);
    let offset = usize::try_from(rng.below(span))
        .unwrap_or(0)
        .saturating_add(1);
    let cut = full.get(..offset).unwrap_or("").to_owned();
    Scenario {
        document: cut,
        model: Model::Json,
    }
}

/// A document with a wrongly typed field is a JSON refusal.
fn wrong_type_scenario(seed: u64) -> Scenario {
    let mut rng = Rng::new(seed);
    let malformed = [
        r#"{"packages":123,"workspace_members":[]}"#,
        r#"{"packages":"nope","workspace_members":[]}"#,
        r#"{"packages":[],"workspace_members":{}}"#,
        r#"{"packages":[],"workspace_members":"later"}"#,
        r#"{"packages":[{"id":"a","name":"a","manifest_path":"/repo/Cargo.toml","dependencies":123}],"workspace_members":["a"]}"#,
        r#"{"packages":[{"id":"a","name":"a","manifest_path":"/repo/Cargo.toml","dependencies":[{"name":"dep","source":"registry+https://example.invalid/index","req":"1.0","optional":"yes"}]}],"workspace_members":["a"]}"#,
    ];
    let document = (*rng.pick(&malformed)).to_owned();
    Scenario {
        document,
        model: Model::Json,
    }
}

/// A virtual workspace with no members is still valid, and has no edges.
fn empty_scenario(_seed: u64) -> Scenario {
    Scenario {
        document: r#"{"packages":[],"workspace_members":[]}"#.to_owned(),
        model: Model::Edges(Vec::new()),
    }
}

// ── Assertion and sweeps ────────────────────────────────────────────────────

/// Asserts `parse` agrees with the model, naming the seed on failure.
fn assert_model(seed: u64, scenario: &Scenario) {
    let observed = observe(metadata::parse(&scenario.document));
    match scenario.model {
        Model::Edges(ref expected) => {
            let mut expected = expected.clone();
            expected.sort();
            let agrees = match observed {
                Observed::Edges(ref actual) => actual == &expected,
                _ => false,
            };
            assert!(
                agrees,
                "seed {seed}: expected {expected:?}, observed {observed:?}"
            );
        }
        Model::Schema(fragment) => {
            let agrees =
                matches!(&observed, Observed::Schema(message) if message.contains(fragment));
            assert!(
                agrees,
                "seed {seed}: expected a schema refusal containing {fragment:?}, observed {observed:?}"
            );
        }
        Model::Json => {
            let agrees = matches!(&observed, Observed::Json);
            assert!(
                agrees,
                "seed {seed}: expected a JSON refusal, observed {observed:?}"
            );
        }
    }
}

/// Runs one scenario family over `0..count`, asserting the model each time.
fn sweep(count: u64, mut scenario: impl FnMut(u64) -> Scenario) -> TestResult {
    for seed in 0..count {
        assert_model(seed, &scenario(seed));
    }
    Ok(())
}

/// Folds eight generated documents for a derived seed into one trace hash.
fn trace(seed: u64) -> u64 {
    let mut hasher = DefaultHasher::new();
    for step in 0..8_u64 {
        let derived = seed.wrapping_mul(8).wrapping_add(step);
        let scenario = valid_scenario(derived, ALL_DIMS);
        scenario.document.hash(&mut hasher);
        let observed = observe(metadata::parse(&scenario.document));
        format!("{observed:?}").hash(&mut hasher);
        assert_model(derived, &scenario);
    }
    hasher.finish()
}

// ── Valid-workspace families ────────────────────────────────────────────────

/// Any mix of dimensions decodes exactly as the model predicts.
#[test]
fn a_seeded_document_matches_the_modelled_edge_set() -> TestResult {
    sweep(64, |seed| valid_scenario(seed, ALL_DIMS))
}

/// The `kind` dimension is decoded and modelled per seed.
#[test]
fn the_kind_dimension_is_modelled_and_decoded() -> TestResult {
    sweep(64, |seed| valid_scenario(seed, &[Dim::Kind]))
}

/// The `target` scope is decoded and modelled per seed.
#[test]
fn the_target_dimension_is_modelled_and_decoded() -> TestResult {
    sweep(64, |seed| valid_scenario(seed, &[Dim::Target]))
}

/// The `optional` bit is decoded and modelled per seed.
#[test]
fn the_optional_dimension_is_modelled_and_decoded() -> TestResult {
    sweep(64, |seed| valid_scenario(seed, &[Dim::Optional]))
}

/// The `default-features` bit is decoded and modelled per seed.
#[test]
fn the_default_features_dimension_is_modelled_and_decoded() -> TestResult {
    sweep(64, |seed| valid_scenario(seed, &[Dim::DefaultFeatures]))
}

/// The enabled `features` list is decoded and modelled per seed.
#[test]
fn the_features_dimension_is_modelled_and_decoded() -> TestResult {
    sweep(64, |seed| valid_scenario(seed, &[Dim::Features]))
}

/// A local rename keeps the upstream identity and is decoded per seed.
#[test]
fn the_rename_dimension_is_modelled_and_decoded() -> TestResult {
    sweep(64, |seed| valid_scenario(seed, &[Dim::Rename]))
}

/// Internal and external path edges are decoded and modelled per seed.
#[test]
fn the_path_dimension_is_modelled_and_decoded() -> TestResult {
    sweep(64, |seed| valid_scenario(seed, &[Dim::Path]))
}

/// A virtual workspace with no members stays valid and yields no edges.
#[test]
fn an_empty_workspace_decodes_to_no_edges() -> TestResult {
    sweep(16, empty_scenario)
}

/// Across the general family the sweep exercises internal member path edges,
/// external path edges, and every dependency kind.
#[test]
fn the_sweep_covers_members_external_paths_and_kinds() -> TestResult {
    let mut saw_internal = false;
    let mut saw_external = false;
    let mut saw_kind = [false; 3];
    for seed in 0..128_u64 {
        let scenario = valid_scenario(seed, ALL_DIMS);
        if let Model::Edges(ref edges) = scenario.model {
            for edge in edges {
                if edge.workspace {
                    saw_internal = true;
                }
                if edge.class == "path" && !edge.workspace {
                    saw_external = true;
                }
                match edge.kind {
                    "build" => saw_kind[1] = true,
                    "dev" => saw_kind[2] = true,
                    _ => saw_kind[0] = true,
                }
            }
        }
        assert_model(seed, &scenario);
    }
    assert!(
        saw_internal && saw_external,
        "the sweep must exercise internal and external path edges"
    );
    assert!(
        saw_kind.iter().all(|seen| *seen),
        "the sweep must exercise normal, build and dev kinds: {saw_kind:?}"
    );
    Ok(())
}

// ── Fault families ──────────────────────────────────────────────────────────

/// A duplicate package record is refused before any graph is built.
#[test]
fn a_duplicate_package_record_is_a_schema_refusal() -> TestResult {
    sweep(64, duplicate_package_scenario)
}

/// A member id with no package record is a schema refusal, not an empty graph.
#[test]
fn a_missing_package_record_is_a_schema_refusal() -> TestResult {
    sweep(64, missing_package_scenario)
}

/// A repeated workspace member id is a schema refusal.
#[test]
fn a_duplicate_member_id_is_a_schema_refusal() -> TestResult {
    sweep(64, duplicate_member_scenario)
}

/// A member with no manifest path is a schema refusal.
#[test]
fn a_member_without_a_manifest_path_is_a_schema_refusal() -> TestResult {
    sweep(64, missing_manifest_scenario)
}

/// A blank identity is a schema refusal.
#[test]
fn a_blank_identity_is_a_schema_refusal() -> TestResult {
    sweep(64, blank_identity_scenario)
}

/// An unknown dependency kind is a schema refusal, never folded to normal.
#[test]
fn an_unknown_dependency_kind_is_a_schema_refusal() -> TestResult {
    sweep(64, unknown_kind_scenario)
}

/// A declaration with neither source nor path is a schema refusal.
#[test]
fn a_dependency_without_source_or_path_is_a_schema_refusal() -> TestResult {
    sweep(64, unsourced_scenario)
}

/// A path target whose name disagrees with its member record is refused.
#[test]
fn a_member_path_name_mismatch_is_a_schema_refusal() -> TestResult {
    sweep(64, mismatch_scenario)
}

/// Two members claiming one manifest directory are refused, never merged.
#[test]
fn two_members_sharing_a_manifest_directory_are_refused() -> TestResult {
    sweep(32, shared_dir_scenario)
}

/// A strict prefix of a valid document is a JSON refusal.
#[test]
fn truncated_documents_are_json_refusals() -> TestResult {
    sweep(64, truncated_scenario)
}

/// A wrongly typed field is a JSON refusal.
#[test]
fn wrongly_typed_fields_are_json_refusals() -> TestResult {
    sweep(64, wrong_type_scenario)
}

/// Replaying a seed produces the same trace hash.
#[test]
fn same_seed_same_trace() -> TestResult {
    for seed in 0..32_u64 {
        let first = trace(seed);
        let second = trace(seed);
        assert_eq!(
            first, second,
            "seed {seed} produced two different trace hashes"
        );
    }
    Ok(())
}
