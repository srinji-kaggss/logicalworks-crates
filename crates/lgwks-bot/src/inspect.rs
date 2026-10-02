//! Typed in-process structural code inspection (R8): check risky code over its
//! structure, never by executing it.
//!
//! An AI that needs risky code checked calls the bot to check it **in process**.
//! This module is that operation. It reads the subject's bytes, asks
//! [`lgwks_ast`] to parse them, and walks the resulting tree against a fixed,
//! versioned structural rule set. It never compiles the subject, never imports
//! it, never runs a build script, never invokes a shell, and never loads a
//! dynamic library from it: the only library it calls is the parser, and the
//! subject's instructions stay data at every step.
//!
//! # The verdict is typed and never free text
//!
//! [`Inspection::verdict`] is one of [`Verdict::Clean`], [`Verdict::Violations`],
//! [`Verdict::Unsupported`], [`Verdict::Undecidable`], [`Verdict::Incomplete`] or
//! [`Verdict::InfrastructureFailure`]. A refusal to decide is a state, not a
//! clean report: a parse that recovered, a budget that ran out, a grammar that
//! is not compiled, and a rule set this build does not implement each land on
//! their own arm, and none of them can be read as "no violation found".
//!
//! # What a clean result is worth
//!
//! [`Verdict::Clean`] means *no configured structural rule matched, within an
//! explicitly complete inspected scope*. It is **not** proof that the subject
//! is free of logical bugs, free of vulnerabilities, or safe to execute. The
//! grammar a subject parsed under is not evidence that a rule speaks its
//! syntax; [`Inspection::coverage`] reports, per rule, whether this build
//! supported it for the observed language, and that is the whole of the claim.
//! [`ASSURANCE`] states the ceiling in one sentence a caller can quote.
//!
//! # Bounds are separate axes
//!
//! Source bytes, parser nodes, traversal depth, rule evaluations and emitted
//! findings are **different** resources and are bounded by different fields on
//! [`Budgets`]. Refusing before avoidable amplification is the rule: the byte
//! bound is checked before the parser runs, the node/depth/work bounds while
//! walking, and the findings/output bounds before a finding is retained.
//! [`Inspection::resources`] reports what was actually charged.
//!
//! This does not claim in-process preemption of arbitrary stuck *foreign* code:
//! a hostile grammar can still spend time inside the parser, which this crate
//! does not own. What it promises is that **this crate** adds no execution of
//! the subject and no unbounded walk of its own.
//!
//! # The parser boundary
//!
//! The parser is third-party Rust (`ast-grep-core` over `tree-sitter`
//! grammars). It is trusted to be memory-safe under `#![forbid(unsafe_code)]`
//! at this crate's own root, but it is not vouched for by this module beyond
//! that: a grammar bug is a parser fault, not an inspection verdict, and
//! [`Verdict::InfrastructureFailure`] is where such a refusal is reported
//! rather than manufactured into a clean result.
//!
//! # Example
//!
//! ```
//! use lgwks_bot::inspect::{inspect, InspectRequest, Verdict};
//!
//! let request = InspectRequest::new("src/lib.rs", "fn f() { let x = g().unwrap(); }");
//! let inspection = inspect(&request);
//! assert!(matches!(inspection.verdict(), Verdict::Violations { .. }));
//! assert_eq!(
//!     inspection.findings().first().map(|found| found.rule_id()),
//!     Some("rust/no-unwrap"),
//!     "a `.unwrap()` is the shipped unwrap rule's whole subject"
//! );
//! ```

use lgwks_ast::{Language, ParseError};
use lgwks_std::hash;
use lgwks_std::json::{Deserialize, Serialize};

/// The wire version of the inspection report.
pub const INSPECTION_VERSION: u32 = 1;

/// The assurance ceiling a clean report carries, in one quotable sentence.
///
/// A finite structural rule set cannot prove a program correct, and a report
/// that let a reader hear otherwise would be the one claim this operation must
/// never make.
pub const ASSURANCE: &str = "structural rule set only: a clean result means no configured \
     structural rule matched within a complete inspected scope, and is not proof the subject is \
     free of logical bugs, free of vulnerabilities, or safe to execute";

/// The most findings one inspection retains before it is marked incomplete.
pub const MAX_FINDINGS: usize = 1_024;

/// The most bytes of one finding's source preview.
pub const MAX_PREVIEW_BYTES: usize = 200;

/// The most bytes of emitted finding text (rule id plus preview) one
/// inspection retains before it is marked incomplete.
pub const MAX_OUTPUT_BYTES: usize = 256 * 1024;

/// The scope a request asks to be inspected.
///
/// One variant today. The intended scope is part of the request so that a
/// clean verdict can name exactly what was covered rather than imply a broader
/// one; a future scope is a new arm, not a reinterpretation of this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Scope {
    /// The subject parsed as a whole, and every configured rule was evaluated
    /// where this build supported it.
    Structural,
}

/// One shipped structural rule set's identity and revision.
///
/// A caller binds a request to the exact rule set it expects; a build that
/// implements a different revision refuses with [`Verdict::Unsupported`]
/// rather than running a rule set the caller did not ask for. Grammar support
/// is not rule support, so the identity here is the rule set's, not a
/// language's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RuleSet {
    /// The stable rule-set name.
    identity: &'static str,
    /// The revision of the rules under that name.
    revision: u32,
}

impl RuleSet {
    /// The structural rule set this build implements.
    pub const STRUCTURAL_V1: RuleSet = RuleSet {
        identity: "lgwks/structural",
        revision: 1,
    };

    /// Bind a request to a named rule set at a specific revision.
    ///
    /// A caller states the rules it expects; a build that implements a
    /// different identity or revision refuses with
    /// [`Verdict::Unsupported`] rather than running rules the caller did not
    /// ask for.
    #[must_use]
    pub const fn new(identity: &'static str, revision: u32) -> Self {
        Self { identity, revision }
    }

    /// The stable rule-set name.
    #[must_use]
    pub const fn identity(self) -> &'static str {
        self.identity
    }

    /// The rule-set revision.
    #[must_use]
    pub const fn revision(self) -> u32 {
        self.revision
    }
}

/// The byte, node, depth, work, findings and output budgets of one inspection.
///
/// Each field is a **separate** resource axis; tightening one does not tighten
/// another. Every field is finite, and every field is readable, so a caller can
/// reason about a refusal without re-running it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Budgets {
    /// Largest subject admitted, checked before the parser runs.
    pub max_source_bytes: usize,
    /// Largest syntax tree walked, checked while walking.
    pub max_nodes: usize,
    /// Deepest node visited, root counting as one.
    pub max_depth: usize,
    /// Most rule evaluations performed, across every visited node.
    pub max_work: usize,
    /// Most findings retained.
    pub max_findings: usize,
    /// Most bytes of emitted finding text retained.
    pub max_output_bytes: usize,
}

impl Budgets {
    /// The shipped defaults: the parser's own source bound, a two-million-node
    /// tree bound, and modest findings/output ceilings.
    #[must_use]
    pub const fn default_budgets() -> Self {
        Self {
            max_source_bytes: lgwks_ast::MAX_SOURCE_BYTES,
            max_nodes: lgwks_ast::MAX_AST_NODES,
            max_depth: 4_096,
            // One evaluation per shipped rule per node, so the default cannot
            // refuse a tree the node bound would have admitted.
            max_work: lgwks_ast::MAX_AST_NODES.saturating_mul(RULES.len()),
            max_findings: MAX_FINDINGS,
            max_output_bytes: MAX_OUTPUT_BYTES,
        }
    }

    /// The default budgets.
    #[must_use]
    pub const fn new() -> Self {
        Self::default_budgets()
    }

    /// Replace the source-byte ceiling.
    #[must_use]
    pub const fn with_source_bytes(mut self, bytes: usize) -> Self {
        self.max_source_bytes = bytes;
        self
    }

    /// Replace the node ceiling.
    #[must_use]
    pub const fn with_nodes(mut self, nodes: usize) -> Self {
        self.max_nodes = nodes;
        self
    }

    /// Replace the depth ceiling.
    #[must_use]
    pub const fn with_depth(mut self, depth: usize) -> Self {
        self.max_depth = depth;
        self
    }

    /// Replace the work ceiling.
    #[must_use]
    pub const fn with_work(mut self, work: usize) -> Self {
        self.max_work = work;
        self
    }

    /// Replace the retained-findings ceiling.
    #[must_use]
    pub const fn with_findings(mut self, findings: usize) -> Self {
        self.max_findings = findings;
        self
    }

    /// Replace the emitted-output ceiling.
    #[must_use]
    pub const fn with_output_bytes(mut self, bytes: usize) -> Self {
        self.max_output_bytes = bytes;
        self
    }
}

impl Default for Budgets {
    /// [`Budgets::default_budgets`].
    fn default() -> Self {
        Self::default_budgets()
    }
}

/// A request to inspect one subject's bytes.
///
/// The subject is borrowed and never copied into the report; only its digest
/// and the bounded previews of its matched spans travel with the result.
#[must_use]
pub struct InspectRequest<'a> {
    /// The artifact identity: what the bytes are, as a path or label. Used to
    /// resolve the language by extension when none is declared, and carried
    /// verbatim into the report.
    artifact: &'a str,
    /// The subject's bytes, as UTF-8 text.
    subject: &'a str,
    /// The language, when the caller already knows it.
    language: Option<Language>,
    /// The language version, when the caller knows it.
    ///
    /// The compiled grammars carry no version identity, so a declared version
    /// cannot be confirmed: a request that names one is
    /// [`Verdict::Undecidable`] rather than checked against whatever grammar
    /// happens to be compiled.
    language_version: Option<&'a str>,
    /// The rule set the caller expects.
    rules: RuleSet,
    /// The scope the caller asks to be inspected.
    scope: Scope,
    /// The bounds to apply.
    budgets: Budgets,
}

impl<'a> InspectRequest<'a> {
    /// Inspect `subject` as the artifact `artifact` under the default rule set,
    /// scope and budgets. The language is resolved from `artifact`'s extension.
    pub const fn new(artifact: &'a str, subject: &'a str) -> Self {
        Self {
            artifact,
            subject,
            language: None,
            language_version: None,
            rules: RuleSet::STRUCTURAL_V1,
            scope: Scope::Structural,
            budgets: Budgets::default_budgets(),
        }
    }

    /// Declare the subject's language, overriding extension detection.
    pub const fn language(mut self, language: Language) -> Self {
        self.language = Some(language);
        self
    }

    /// Declare the language version the subject is written against.
    ///
    /// Recorded so a report can be bound to the caller's expectation; a
    /// declared version makes the inspection
    /// [`Verdict::Undecidable`] because the compiled grammar cannot confirm it.
    pub const fn language_version(mut self, version: &'a str) -> Self {
        self.language_version = Some(version);
        self
    }

    /// Bind the request to a specific rule set.
    pub const fn rules(mut self, rules: RuleSet) -> Self {
        self.rules = rules;
        self
    }

    /// Declare the scope to inspect.
    pub const fn scope(mut self, scope: Scope) -> Self {
        self.scope = scope;
        self
    }

    /// Replace the budgets.
    pub const fn budgets(mut self, budgets: Budgets) -> Self {
        self.budgets = budgets;
        self
    }
}

/// Why this build could not inspect the subject at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
#[non_exhaustive]
pub enum UnsupportedReason {
    /// No compiled grammar claims the artifact, or the language was declared
    /// but this build has no grammar for it.
    NoCompiledGrammar {
        /// The artifact identity the request named.
        artifact: String,
    },
    /// The rule set name is not one this build implements.
    UnknownRuleSet {
        /// The rule-set name the request bound.
        identity: String,
    },
    /// The rule set is known but a different revision.
    RuleSetRevision {
        /// The revision the request bound.
        found: u32,
        /// The revision this build implements.
        supported: u32,
    },
    /// The language is compiled, but the shipped rule set speaks none of its
    /// syntax, so no rule could be evaluated against it.
    RulesNotSupportedForLanguage {
        /// The observed language name.
        language: String,
    },
}

/// Why an inspection stopped before it could decide the whole subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
#[non_exhaustive]
pub enum IncompleteReason {
    /// The subject exceeded the source-byte budget before the parser ran.
    SourceTooLarge {
        /// Observed length in bytes.
        actual: usize,
        /// The applied bound.
        limit: usize,
    },
    /// The parser recovered from invalid syntax, so the tree is not trustworthy
    /// from the first recovery node onward.
    SyntaxRecovery {
        /// Recovery nodes observed by the checked parse.
        recovery_nodes: usize,
        /// Whether more recovery nodes were observed than retained.
        diagnostics_truncated: bool,
    },
    /// The tree exceeded the node budget.
    NodeBudgetExceeded {
        /// Nodes observed up to the budget's overflow witness.
        observed: usize,
        /// The applied bound.
        limit: usize,
    },
    /// The walk reached deeper than the depth budget.
    DepthBudgetExceeded {
        /// Depth reached.
        reached: usize,
        /// The applied bound.
        limit: usize,
    },
    /// The walk performed more rule evaluations than the work budget allows.
    WorkBudgetExceeded {
        /// Evaluations observed up to the budget's overflow witness.
        observed: usize,
        /// The applied bound.
        limit: usize,
    },
    /// More findings were produced than the findings budget retains.
    FindingsBudgetExceeded {
        /// The applied bound.
        limit: usize,
    },
    /// Emitted finding text exceeded the output-byte budget.
    OutputBudgetExceeded {
        /// The applied bound.
        limit: usize,
    },
}

/// One rule's coverage in one inspection.
///
/// `supported` says whether this build speaks the rule for the observed
/// language; `evaluated` says whether the walk reached the point of evaluating
/// it. A rule can be supported and not evaluated (a parse refusal), or
/// unsupported and not evaluated (the wrong language). Neither is silence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
#[non_exhaustive]
pub struct RuleCoverage {
    /// The stable rule id.
    rule_id: String,
    /// Whether this build supports the rule for the observed language.
    supported: bool,
    /// Whether the rule was actually evaluated.
    evaluated: bool,
    /// The syntax the rule inspects, in one line.
    evidence: String,
}

impl RuleCoverage {
    /// The stable rule id.
    #[must_use]
    pub fn rule_id(&self) -> &str {
        &self.rule_id
    }

    /// Whether this build supports the rule for the observed language.
    #[must_use]
    pub const fn supported(&self) -> bool {
        self.supported
    }

    /// Whether the rule was actually evaluated.
    #[must_use]
    pub const fn evaluated(&self) -> bool {
        self.evaluated
    }

    /// The syntax the rule inspects.
    #[must_use]
    pub fn evidence(&self) -> &str {
        &self.evidence
    }
}

/// One detected violation, with an exact byte span and a bounded preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
#[non_exhaustive]
pub struct Finding {
    /// The stable rule id that matched.
    rule_id: String,
    /// Inclusive UTF-8 byte offset of the matched node in the subject.
    start_byte: usize,
    /// Exclusive UTF-8 byte offset of the matched node in the subject.
    end_byte: usize,
    /// A bounded preview of the matched text.
    preview: String,
    /// Whether the preview was truncated to [`MAX_PREVIEW_BYTES`].
    preview_truncated: bool,
}

impl Finding {
    /// The stable rule id that matched.
    #[must_use]
    pub fn rule_id(&self) -> &str {
        &self.rule_id
    }

    /// The matched node's byte range.
    #[must_use]
    pub const fn byte_range(&self) -> (usize, usize) {
        (self.start_byte, self.end_byte)
    }

    /// The bounded preview of the matched text.
    #[must_use]
    pub fn preview(&self) -> &str {
        &self.preview
    }

    /// Whether the preview was truncated.
    #[must_use]
    pub const fn preview_truncated(&self) -> bool {
        self.preview_truncated
    }
}

/// What was charged against the budgets, for a caller reasoning about a cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
#[non_exhaustive]
pub struct Resources {
    /// Nodes visited.
    pub nodes: usize,
    /// Deepest node visited.
    pub max_depth: usize,
    /// Rule evaluations performed.
    pub work: usize,
    /// Findings retained.
    pub findings: usize,
    /// Bytes of finding text retained.
    pub output_bytes: usize,
}

/// How an inspection ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Verdict {
    /// No configured rule matched, within a complete supported scope.
    Clean {
        /// The scope that was completely inspected.
        scope: Scope,
    },
    /// One or more violations were found.
    Violations {
        /// The number of violations retained.
        count: usize,
    },
    /// This build could not inspect the subject at all.
    Unsupported {
        /// Why.
        reason: UnsupportedReason,
    },
    /// The inspection ran but a property could not be decided.
    Undecidable {
        /// The undecidable property, in one line.
        property: String,
    },
    /// The inspection ran but did not cover the whole subject.
    Incomplete {
        /// Why.
        reason: IncompleteReason,
    },
    /// The parser or another infrastructure dependency failed.
    InfrastructureFailure {
        /// The failure, in one line.
        cause: String,
    },
}

/// The typed result of one inspection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
#[non_exhaustive]
pub struct Inspection {
    /// The report's wire version.
    version: u32,
    /// The artifact identity the request named.
    artifact: String,
    /// The subject's content digest, so a report binds to exact bytes.
    subject_digest: String,
    /// The observed language name, when one was resolved.
    language: Option<String>,
    /// The rule set the request bound.
    rule_identity: String,
    /// The rule set's revision.
    rule_revision: u32,
    /// The scope the request asked for.
    scope: Scope,
    /// The verdict.
    verdict: Verdict,
    /// Per-rule coverage.
    coverage: Vec<RuleCoverage>,
    /// Detected violations.
    findings: Vec<Finding>,
    /// What was charged against the budgets.
    resources: Resources,
}

impl Inspection {
    /// The report's wire version.
    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    /// The artifact identity the request named.
    #[must_use]
    pub fn artifact(&self) -> &str {
        &self.artifact
    }

    /// The subject's content digest.
    #[must_use]
    pub fn subject_digest(&self) -> &str {
        &self.subject_digest
    }

    /// The observed language name, when one was resolved.
    #[must_use]
    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    /// The rule-set identity.
    #[must_use]
    pub fn rule_identity(&self) -> &str {
        &self.rule_identity
    }

    /// The rule-set revision.
    #[must_use]
    pub const fn rule_revision(&self) -> u32 {
        self.rule_revision
    }

    /// The scope the request asked for.
    #[must_use]
    pub const fn scope(&self) -> Scope {
        self.scope
    }

    /// The verdict.
    #[must_use]
    pub const fn verdict(&self) -> &Verdict {
        &self.verdict
    }

    /// Per-rule coverage.
    #[must_use]
    pub fn coverage(&self) -> &[RuleCoverage] {
        &self.coverage
    }

    /// Detected violations.
    #[must_use]
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    /// What was charged against the budgets.
    #[must_use]
    pub const fn resources(&self) -> Resources {
        self.resources
    }

    /// The assurance ceiling a clean result carries. See [`ASSURANCE`].
    #[must_use]
    pub const fn assurance() -> &'static str {
        ASSURANCE
    }

    /// Serialize the report as JSON through the shared facade.
    ///
    /// # Errors
    ///
    /// The facade's serialization error, which for these owned types is a
    /// formatting failure rather than a schema one.
    pub fn to_json(&self) -> Result<String, crate::json::Error> {
        crate::json::to_string(self)
    }
}

/// One shipped structural rule.
struct Rule {
    /// The stable rule id.
    rule_id: &'static str,
    /// The syntax the rule inspects, in one line.
    evidence: &'static str,
}

/// The shipped rule set, in report order.
const RULES: &[Rule] = &[
    Rule {
        rule_id: "rust/no-unwrap",
        evidence: "an identifier named `unwrap` or `expect` (a fallible-operation panic site)",
    },
    Rule {
        rule_id: "rust/no-todo",
        evidence: "a `todo!` or `unimplemented!` macro invocation",
    },
    Rule {
        rule_id: "rust/no-panic",
        evidence: "a `panic!` macro invocation",
    },
];

/// The language the shipped rules speak.
fn rules_speak(language: Language) -> bool {
    language == Language::Rust
}

/// Whether `rule_id` matches the node described by `node_kind` and `node_text`.
fn rule_matches(rule_id: &str, node_kind: &str, node_text: &str) -> bool {
    match rule_id {
        "rust/no-unwrap" => {
            matches!(node_kind, "identifier" | "field_identifier")
                && matches!(node_text, "unwrap" | "expect")
        }
        "rust/no-todo" => {
            node_kind == "macro_invocation"
                && matches!(macro_head(node_text), "todo" | "unimplemented")
        }
        "rust/no-panic" => node_kind == "macro_invocation" && macro_head(node_text) == "panic",
        _ => false,
    }
}

/// The bare macro name at the head of a macro invocation's text.
///
/// `todo!("x")` yields `todo`; the optional space before `!` that some
/// formatters insert is absorbed. Anything with no head yields `""`, which
/// matches no rule.
fn macro_head(node_text: &str) -> &str {
    node_text
        .split(['!', '(', ' ', '\t', '\n'])
        .next()
        .unwrap_or("")
}

/// A bounded, char-boundary-safe preview of `source[start..end]`.
fn bounded_preview(source: &str, start: usize, end: usize) -> (String, bool) {
    let slice = source.get(start..end).unwrap_or("");
    let cutoff = slice
        .char_indices()
        .nth(MAX_PREVIEW_BYTES)
        .map_or(slice.len(), |(at, _)| at);
    let truncated = cutoff < slice.len();
    (slice[..cutoff].to_owned(), truncated)
}

/// The parser this operation calls, as a seam.
///
/// The shipped value is [`lgwks_ast::try_parse`], and the only other value is a
/// fault injected by the crate's own tests: R8 requires the
/// [`Verdict::InfrastructureFailure`] arm to be exercised by a producer, not
/// merely declared, and the compiled Rust grammar never fails. The seam is
/// crate-private, so no caller can substitute a parser and no second parsing
/// implementation exists.
type ParseFn = fn(&str, Language) -> Result<lgwks_ast::Parsed, ParseError>;

/// Inspect `request`'s subject in process and return a typed report.
///
/// This is the whole operation: parse, walk, decide. It performs no effect
/// outside this process, spawns no task, opens no file and runs no subject
/// code.
#[must_use]
pub fn inspect(request: &InspectRequest<'_>) -> Inspection {
    inspect_with(request, lgwks_ast::try_parse)
}

/// The whole operation, parameterized on the parser it calls.
///
/// [`inspect`] passes the shipped parser; the crate's own tests pass a fault to
/// exercise [`Verdict::InfrastructureFailure`]. Every budget is enforced.
fn inspect_with(request: &InspectRequest<'_>, parse: ParseFn) -> Inspection {
    inspect_mode(request, parse, true)
}

/// The whole operation, with the budget walk optionally disarmed.
///
/// `enforce` is `true` for every shipped path. The eager mutant the budget
/// oracles are tested against is `false`: it still parses, counts and records,
/// but never stops on a budget, so a test can show the behavioral oracle rejects
/// it. It is `#[cfg(test)]` and cannot be reached from a shipped build.
#[cfg(test)]
fn inspect_eager(request: &InspectRequest<'_>) -> Inspection {
    inspect_mode(request, lgwks_ast::try_parse, false)
}

/// Run one inspection, parsing through `parse` and enforcing budgets when
/// `enforce` is set.
fn inspect_mode(request: &InspectRequest<'_>, parse: ParseFn, enforce: bool) -> Inspection {
    let digest = hash::blake3(request.subject.as_bytes()).to_hex();
    let coverage = coverage_for(&request.rules, request.language, false);
    let base = |verdict: Verdict,
                language: Option<String>,
                coverage: Vec<RuleCoverage>,
                findings: Vec<Finding>,
                resources: Resources| Inspection {
        version: INSPECTION_VERSION,
        artifact: request.artifact.to_owned(),
        subject_digest: digest.clone(),
        language,
        rule_identity: request.rules.identity().to_owned(),
        rule_revision: request.rules.revision(),
        scope: request.scope,
        verdict,
        coverage,
        findings,
        resources,
    };
    let empty = Resources {
        nodes: 0,
        max_depth: 0,
        work: 0,
        findings: 0,
        output_bytes: 0,
    };

    // A rule set this build does not implement is refused before any parse: the
    // caller asked for rules that do not exist here, and running different ones
    // would answer a different question.
    if request.rules.identity() != RuleSet::STRUCTURAL_V1.identity() {
        return base(
            Verdict::Unsupported {
                reason: UnsupportedReason::UnknownRuleSet {
                    identity: request.rules.identity().to_owned(),
                },
            },
            None,
            coverage,
            Vec::new(),
            empty,
        );
    }
    if request.rules.revision() != RuleSet::STRUCTURAL_V1.revision() {
        return base(
            Verdict::Unsupported {
                reason: UnsupportedReason::RuleSetRevision {
                    found: request.rules.revision(),
                    supported: RuleSet::STRUCTURAL_V1.revision(),
                },
            },
            None,
            coverage,
            Vec::new(),
            empty,
        );
    }

    // Resolve the language. An explicit language wins; otherwise the artifact's
    // extension chooses.
    let resolved = request
        .language
        .or_else(|| lgwks_ast::detect(request.artifact));
    let Some(language) = resolved else {
        return base(
            Verdict::Unsupported {
                reason: UnsupportedReason::NoCompiledGrammar {
                    artifact: request.artifact.to_owned(),
                },
            },
            None,
            coverage,
            Vec::new(),
            empty,
        );
    };
    let language_name = language.name().to_owned();
    // Coverage is reported as supported-but-not-yet-evaluated until a parse
    // succeeds; a refusal must not claim any rule ran.
    let coverage = coverage_for(&request.rules, Some(language), false);

    // A declared language version cannot be confirmed: the compiled grammar
    // carries no version identity, so the operation says the property is
    // undecidable rather than checking the subject against a version it cannot
    // name. This is the "cannot decide returns undecidable" arm of R8.
    if let Some(version) = request.language_version {
        return base(
            Verdict::Undecidable {
                property: format!(
                    "language version `{version}`: the compiled grammar carries no version \
                     identity, so this build will not claim to have checked that exact version"
                ),
            },
            Some(language_name),
            coverage,
            Vec::new(),
            empty,
        );
    }

    // A compiled grammar the rules do not speak is not a clean subject; it is a
    // subject this rule set cannot see.
    if !rules_speak(language) {
        return base(
            Verdict::Unsupported {
                reason: UnsupportedReason::RulesNotSupportedForLanguage {
                    language: language_name.clone(),
                },
            },
            Some(language_name),
            coverage,
            Vec::new(),
            empty,
        );
    }

    // The byte bound is checked before the parser is handed anything.
    if request.subject.len() > request.budgets.max_source_bytes {
        return base(
            Verdict::Incomplete {
                reason: IncompleteReason::SourceTooLarge {
                    actual: request.subject.len(),
                    limit: request.budgets.max_source_bytes,
                },
            },
            Some(language_name),
            coverage,
            Vec::new(),
            empty,
        );
    }

    let parsed = match parse(request.subject, language) {
        Ok(parsed) => parsed,
        Err(ParseError::InvalidSyntax {
            diagnostics,
            diagnostics_truncated,
            ..
        }) => {
            return base(
                Verdict::Incomplete {
                    reason: IncompleteReason::SyntaxRecovery {
                        recovery_nodes: diagnostics.len(),
                        diagnostics_truncated,
                    },
                },
                Some(language_name),
                coverage,
                Vec::new(),
                empty,
            );
        }
        Err(ParseError::SourceTooLarge { actual, limit }) => {
            return base(
                Verdict::Incomplete {
                    reason: IncompleteReason::SourceTooLarge { actual, limit },
                },
                Some(language_name),
                coverage,
                Vec::new(),
                empty,
            );
        }
        Err(ParseError::AstTooLarge {
            observed, limit, ..
        }) => {
            return base(
                Verdict::Incomplete {
                    reason: IncompleteReason::NodeBudgetExceeded { observed, limit },
                },
                Some(language_name),
                coverage,
                Vec::new(),
                empty,
            );
        }
        Err(ParseError::ParserUnavailable { language, detail }) => {
            return base(
                Verdict::InfrastructureFailure {
                    cause: format!("{language} parser unavailable: {detail}"),
                },
                Some(language_name),
                coverage,
                Vec::new(),
                empty,
            );
        }
        // `ParseError` is `#[non_exhaustive]`: a refusal variant added upstream
        // is an infrastructure fault this build cannot classify, and it is
        // reported as one rather than folded into a clean verdict.
        Err(other) => {
            return base(
                Verdict::InfrastructureFailure {
                    cause: other.to_string(),
                },
                Some(language_name),
                coverage,
                Vec::new(),
                empty,
            );
        }
    };

    // The parse succeeded, so every supported rule is now evaluated.
    let coverage = coverage_for(&request.rules, Some(language), true);
    let walk = walk(&parsed, request.subject, &request.budgets, enforce);
    let resources = Resources {
        nodes: walk.nodes,
        max_depth: walk.max_depth,
        work: walk.work,
        findings: walk.findings.len(),
        output_bytes: walk.output_bytes,
    };
    // Findings and the verdict are independent: an incomplete walk still
    // carries every finding it retained, and the verdict names the incompleteness
    // rather than a clean scope.
    let verdict = match walk.incomplete {
        Some(reason) => Verdict::Incomplete { reason },
        None if walk.findings.is_empty() => Verdict::Clean {
            scope: request.scope,
        },
        None => Verdict::Violations {
            count: walk.findings.len(),
        },
    };
    base(
        verdict,
        Some(language_name),
        coverage,
        walk.findings,
        resources,
    )
}

/// Per-rule coverage for `rules` under `language`.
fn coverage_for(rules: &RuleSet, language: Option<Language>, parsed: bool) -> Vec<RuleCoverage> {
    // A rule set other than the shipped one has no coverage to describe.
    let known = rules.identity() == RuleSet::STRUCTURAL_V1.identity()
        && rules.revision() == RuleSet::STRUCTURAL_V1.revision();
    RULES
        .iter()
        .map(|rule| {
            let supported = known && language.is_some_and(rules_speak);
            RuleCoverage {
                rule_id: rule.rule_id.to_owned(),
                supported,
                evaluated: supported && parsed,
                evidence: rule.evidence.to_owned(),
            }
        })
        .collect()
}

/// What one bounded walk accumulated.
struct WalkOutcome {
    /// Nodes visited.
    nodes: usize,
    /// Deepest node visited.
    max_depth: usize,
    /// Rule evaluations performed.
    work: usize,
    /// Findings retained.
    findings: Vec<Finding>,
    /// Bytes of finding text retained.
    output_bytes: usize,
    /// Why the walk stopped early, if it did.
    incomplete: Option<IncompleteReason>,
}

/// One visited node's frame: the node, its remaining child index, and its depth.
type Frame<'t> = (lgwks_ast::AstNode<'t>, usize, usize);

/// Walk `parsed` against the shipped rules under `budgets`.
///
/// The walk is iterative with one frame per active ancestor, so its resident
/// state follows depth rather than sibling fan-out, and every budget is charged
/// as the work happens rather than after it.
fn walk(parsed: &lgwks_ast::Parsed, source: &str, budgets: &Budgets, enforce: bool) -> WalkOutcome {
    let root = parsed.root();
    let mut outcome = WalkOutcome {
        nodes: 0,
        max_depth: 0,
        work: 0,
        findings: Vec::new(),
        output_bytes: 0,
        incomplete: None,
    };

    // The root is visited first and charged like any other node, so a budget
    // that cannot even admit the root is a refusal rather than an empty walk.
    if !charge_and_evaluate(&root, 1, source, budgets, &mut outcome, enforce) {
        return outcome;
    }
    let root_children = root.children().len();
    let mut frames: Vec<Frame<'_>> = vec![(root, root_children, 1)];
    while let Some(frame) = frames.last_mut() {
        let Some(child_index) = frame.1.checked_sub(1) else {
            frames.pop();
            continue;
        };
        frame.1 = child_index;
        let next_child = frame.0.child(child_index);
        let Some(child) = next_child else {
            continue;
        };
        let depth = frame.2.saturating_add(1);
        if !charge_and_evaluate(&child, depth, source, budgets, &mut outcome, enforce) {
            return outcome;
        }
        let child_count = child.children().len();
        frames.push((child, child_count, depth));
    }
    outcome
}

/// Charge one node against every budget and evaluate the shipped rules on it.
///
/// Returns `false` when a budget stopped the walk, having recorded the reason
/// on `outcome`.
fn charge_and_evaluate(
    node: &lgwks_ast::AstNode<'_>,
    depth: usize,
    source: &str,
    budgets: &Budgets,
    outcome: &mut WalkOutcome,
    enforce: bool,
) -> bool {
    outcome.nodes = outcome.nodes.saturating_add(1);
    if enforce && outcome.nodes > budgets.max_nodes {
        outcome.incomplete = Some(IncompleteReason::NodeBudgetExceeded {
            observed: outcome.nodes,
            limit: budgets.max_nodes,
        });
        return false;
    }
    if enforce && depth > budgets.max_depth {
        outcome.max_depth = outcome.max_depth.max(depth);
        outcome.incomplete = Some(IncompleteReason::DepthBudgetExceeded {
            reached: depth,
            limit: budgets.max_depth,
        });
        return false;
    }
    outcome.max_depth = outcome.max_depth.max(depth);

    // The node's kind and text borrow the tree, so they are held for the whole
    // evaluation and never copied.
    let kind = node.kind();
    let node_kind: &str = kind.as_ref();
    let text = node.text();
    let node_text: &str = text.as_ref();
    for rule in RULES {
        outcome.work = outcome.work.saturating_add(1);
        if enforce && outcome.work > budgets.max_work {
            outcome.incomplete = Some(IncompleteReason::WorkBudgetExceeded {
                observed: outcome.work,
                limit: budgets.max_work,
            });
            return false;
        }
        if rule_matches(rule.rule_id, node_kind, node_text)
            && !retain_finding(rule, node, source, budgets, outcome, enforce)
        {
            return false;
        }
    }
    true
}

/// Retain one finding, charging the findings and output budgets first.
///
/// Returns `false` when a budget stopped the walk.
fn retain_finding(
    rule: &Rule,
    node: &lgwks_ast::AstNode<'_>,
    source: &str,
    budgets: &Budgets,
    outcome: &mut WalkOutcome,
    enforce: bool,
) -> bool {
    if enforce && outcome.findings.len() >= budgets.max_findings {
        outcome.incomplete = Some(IncompleteReason::FindingsBudgetExceeded {
            limit: budgets.max_findings,
        });
        return false;
    }
    let range = node.range();
    let (preview, preview_truncated) = bounded_preview(source, range.start, range.end);
    let cost = rule.rule_id.len().saturating_add(preview.len());
    if enforce && outcome.output_bytes.saturating_add(cost) > budgets.max_output_bytes {
        outcome.incomplete = Some(IncompleteReason::OutputBudgetExceeded {
            limit: budgets.max_output_bytes,
        });
        return false;
    }
    outcome.output_bytes = outcome.output_bytes.saturating_add(cost);
    outcome.findings.push(Finding {
        rule_id: rule.rule_id.to_owned(),
        start_byte: range.start,
        end_byte: range.end,
        preview,
        preview_truncated,
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wide subject, with one violation per line.
    fn wide_subject() -> String {
        (0..64)
            .map(|index| format!("fn f{index}() {{ let x = unwrap(); }}\n"))
            .collect()
    }

    /// R8: an infrastructure failure is its own arm, never a clean verdict.
    ///
    /// The compiled Rust grammar does not fail, so the crate-private parser
    /// seam is the only producer of this arm. Injecting
    /// [`ParseError::ParserUnavailable`] exercises the `Incomplete`-versus-
    /// `InfrastructureFailure` fork by name: the operation reports the failure
    /// and retains no finding, and never returns `Clean`.
    #[test]
    fn a_parser_fault_is_an_infrastructure_failure_not_a_clean_report()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = InspectRequest::new("src/lib.rs", "fn f() { g().unwrap(); }");
        let faulting: ParseFn = |_code, language| {
            Err(ParseError::ParserUnavailable {
                language: language.name(),
                detail: "injected fault: no compiled parser".to_owned(),
            })
        };
        let report = inspect_with(&request, faulting);
        // Owned match: `report.verdict()` borrows, and binding a field out of a
        // borrowed `#[non_exhaustive]` variant is the shape
        // `clippy::pattern_type_mismatch` refuses. The clone is one small test
        // value, which is cheaper than a suppression.
        match report.verdict().clone() {
            Verdict::InfrastructureFailure { cause } => assert!(
                cause.contains("injected fault"),
                "the failure must carry the parser's own detail: {cause:?}"
            ),
            other => {
                return Err(format!("expected an infrastructure failure, got {other:?}").into());
            }
        }
        assert!(
            report.findings().is_empty(),
            "an infrastructure failure retains no finding"
        );
        assert!(
            !matches!(report.verdict(), Verdict::Clean { .. }),
            "an infrastructure failure must never read as clean"
        );
        Ok(())
    }

    /// R8: an eager-traversal mutant fails the budget oracle.
    ///
    /// `inspect_eager` parses and walks exactly as the shipped operation does,
    /// but does not stop when a budget is exhausted. A behavioral oracle — a
    /// tiny node budget over a wide subject must refuse — rejects it where the
    /// shipped operation passes the same oracle, which is the negative control
    /// the issue asks for: the check is the returned verdict and the charged
    /// node count, not the absence of a mutant API.
    #[test]
    fn an_eager_traversal_mutant_fails_the_node_budget_oracle() {
        let source = wide_subject();
        let request = InspectRequest::new("wide.rs", &source).budgets(Budgets::new().with_nodes(3));

        let shipped = inspect(&request);
        assert!(
            matches!(
                shipped.verdict(),
                Verdict::Incomplete {
                    reason: IncompleteReason::NodeBudgetExceeded { .. }
                }
            ),
            "the oracle the mutant must fail: {:?}",
            shipped.verdict()
        );
        assert!(
            shipped.resources().nodes <= 4,
            "the shipped walk stops at the bound plus its overflow witness: {}",
            shipped.resources().nodes
        );

        let mutant = inspect_eager(&request);
        assert!(
            !matches!(
                mutant.verdict(),
                Verdict::Incomplete {
                    reason: IncompleteReason::NodeBudgetExceeded { .. }
                }
            ),
            "the counterfactual control: the eager walk must not report the budget refusal \
             the shipped walk reports: {:?}",
            mutant.verdict()
        );
        assert!(
            mutant.resources().nodes > 3,
            "and it must have walked past the cap, which is what makes the oracle behavioral: {}",
            mutant.resources().nodes
        );
    }

    /// R8: a subject-executing mutant fails the non-execution oracle.
    ///
    /// The mutant runs the subject — here a closure that performs the very
    /// effect the subject's text describes — before inspecting. The oracle is
    /// an independent filesystem observer: the marker the subject would delete
    /// is gone after the mutant runs and present after the shipped operation,
    /// so the oracle distinguishes the two by observed effect and not by an
    /// API that happens to be absent.
    #[test]
    fn a_subject_executing_mutant_fails_the_non_execution_oracle()
    -> Result<(), Box<dyn std::error::Error>> {
        let marker =
            std::env::temp_dir().join(format!("lgwks-inspect-mutant-{}", std::process::id()));
        let shown = marker.display();
        let source = format!("fn main() {{ std::fs::remove_file(\"{shown}\").unwrap(); }}\n");
        let request = InspectRequest::new("src/main.rs", &source);

        // The shipped operation never runs the subject: the marker it "would"
        // delete is untouched.
        std::fs::write(&marker, b"present")?;
        let shipped = inspect(&request);
        assert!(
            marker.exists(),
            "the shipped operation must not execute the subject: {:?}",
            shipped.verdict()
        );

        // The executing mutant runs the subject's effect, and the same oracle
        // that passed above now observes the marker gone.
        let mutant = inspect_executing(&request, |_subject| {
            let _removed = std::fs::remove_file(&marker);
        });
        assert!(
            !marker.exists(),
            "the executing mutant must trip the marker observer: {:?}",
            mutant.verdict()
        );
        let _cleanup = std::fs::remove_file(&marker);
        Ok(())
    }

    /// A test-only variant that executes the subject before inspecting.
    ///
    /// The `run_subject` closure stands in for whatever "execute" means; the
    /// point is that the operation performs it, so the filesystem oracle has
    /// something to observe. Never compiled outside tests.
    fn inspect_executing<F: FnOnce(&str)>(
        request: &InspectRequest<'_>,
        run_subject: F,
    ) -> Inspection {
        run_subject(request.subject);
        inspect(request)
    }
}
