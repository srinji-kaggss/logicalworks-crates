//! The vocabulary of one inspection: the request and its budgets, the verdict and the reasons behind it,
//! and the typed report. Split from `inspect.rs` (#526) so each file keeps to one responsibility.

use lgwks_ast::Language;
use lgwks_std::json::{Deserialize, Serialize};

use super::rules::RULES;
use super::{ASSURANCE, MAX_FINDINGS, MAX_OUTPUT_BYTES};

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

    /// Return these budgets with the largest admitted subject set to `bytes`; a larger subject is refused before the parser runs.
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
    pub(super) artifact: &'a str,
    /// The subject's bytes, as UTF-8 text.
    pub(super) subject: &'a str,
    /// The language, when the caller already knows it.
    pub(super) language: Option<Language>,
    /// The language version, when the caller knows it.
    ///
    /// The compiled grammars carry no version identity, so a declared version
    /// cannot be confirmed: a request that names one is
    /// [`Verdict::Undecidable`] rather than checked against whatever grammar
    /// happens to be compiled.
    pub(super) language_version: Option<&'a str>,
    /// The rule set the caller expects.
    pub(super) rules: RuleSet,
    /// The scope the caller asks to be inspected.
    pub(super) scope: Scope,
    /// The bounds to apply.
    pub(super) budgets: Budgets,
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

    /// Return the request with all six resource ceilings replaced by `budgets`; every other setting is kept.
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
    /// The parser was still working when its deadline passed and was stopped,
    /// so there is no tree to inspect. A statement about the subject's cost
    /// for this grammar, not about its syntax.
    ParseDeadlineExceeded {
        /// The deadline the parser ran under, in milliseconds.
        deadline_ms: u64,
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
    pub(super) rule_id: String,
    /// Whether this build supports the rule for the observed language.
    pub(super) supported: bool,
    /// Whether the rule was actually evaluated.
    pub(super) evaluated: bool,
    /// The syntax the rule inspects, in one line.
    pub(super) evidence: String,
}

impl RuleCoverage {
    /// The stable identifier the rule is registered under, which receipts and coverage tables quote verbatim.
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
    pub(super) rule_id: String,
    /// Inclusive UTF-8 byte offset of the matched node in the subject.
    pub(super) start_byte: usize,
    /// Exclusive UTF-8 byte offset of the matched node in the subject.
    pub(super) end_byte: usize,
    /// A bounded preview of the matched text.
    pub(super) preview: String,
    /// Whether the preview was truncated to [`MAX_PREVIEW_BYTES`](super::MAX_PREVIEW_BYTES).
    pub(super) preview_truncated: bool,
}

impl Finding {
    /// The stable identifier of the rule that produced this match, as registered, so a finding can be traced to its rule.
    #[must_use]
    pub fn rule_id(&self) -> &str {
        &self.rule_id
    }

    /// Where the matched node sits in the subject, as a start offset and an exclusive end offset in bytes.
    #[must_use]
    pub const fn byte_range(&self) -> (usize, usize) {
        (self.start_byte, self.end_byte)
    }

    /// The bounded preview of the matched text.
    #[must_use]
    pub fn preview(&self) -> &str {
        &self.preview
    }

    /// True when the matched text was longer than the preview ceiling, so the preview holds only its leading part.
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
    pub(super) version: u32,
    /// The artifact identity the request named.
    pub(super) artifact: String,
    /// The subject's content digest, so a report binds to exact bytes.
    pub(super) subject_digest: String,
    /// The observed language name, when one was resolved.
    pub(super) language: Option<String>,
    /// The rule set the request bound.
    pub(super) rule_identity: String,
    /// The rule set's revision.
    pub(super) rule_revision: u32,
    /// The scope the request asked for.
    pub(super) scope: Scope,
    /// The verdict.
    pub(super) verdict: Verdict,
    /// Per-rule coverage.
    pub(super) coverage: Vec<RuleCoverage>,
    /// Detected violations.
    pub(super) findings: Vec<Finding>,
    /// What was charged against the budgets.
    pub(super) resources: Resources,
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

    /// The digest of the subject's exact bytes, which names the text the verdict was reached over.
    #[must_use]
    pub fn subject_digest(&self) -> &str {
        &self.subject_digest
    }

    /// The observed language name, when one was resolved.
    #[must_use]
    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    /// The identity of the rule set that was applied, which together with the revision pins what the verdict means.
    #[must_use]
    pub fn rule_identity(&self) -> &str {
        &self.rule_identity
    }

    /// The revision of that rule set, which together with its identity pins which rules produced the verdict.
    #[must_use]
    pub const fn rule_revision(&self) -> u32 {
        self.rule_revision
    }

    /// The scope the request asked for.
    #[must_use]
    pub const fn scope(&self) -> Scope {
        self.scope
    }

    /// The overall result of the inspection: clean, violations found, or a subject this build could not inspect.
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
