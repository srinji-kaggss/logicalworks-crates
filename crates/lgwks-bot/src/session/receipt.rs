//! Decision receipts, the journal trait, the memory journal and the session id. Split from `session.rs` (#526).

use super::{Deserialize, NodeId, Provenance, Resolution, Serialize, fmt};

/// Schema version of the [`DecisionReceipt`] this crate writes.
///
/// Bumped when a field's meaning changes or a field is removed. A reader that
/// finds a version it does not know can refuse the record rather than
/// misinterpret it, which is the only reason to version it at all: a receipt
/// outlives the code that wrote it, and the code that reads it is not always
/// the same revision.
pub const RECEIPT_VERSION: u32 = 1;

/// One structured record of a decision, written as part of the transition it
/// describes.
///
/// The receipt exists because a transcript cannot be the audit record. A
/// transcript is text: it says a person said something and the session moved,
/// and it cannot say which option was considered, on whose authority the
/// comparison was made, or whether a score of nothing meant *no similarity* or
/// *no measurement*. Those are the questions a run has to be able to answer
/// afterwards, and each field here is one of them:
///
/// - `session` and `flow` — which session, running which revision of the flow.
///   A receipt that names a node but not the document it came from cannot be
///   read against the flow a reader has on disk.
/// - `node` and `options` — where the decision was taken, and the *ordered*
///   candidate set it was taken against. The option list is digested rather
///   than counted, because a reordering is a different question with a
///   different right answer.
/// - `resolution` — the verdict verbatim, including the tier. A score is not
///   comparable across tiers, so a receipt holding a score without its tier
///   holds an unreadable number.
/// - `provenance` — the policy in force, and the model when one was consulted.
/// - `selected` and `route` — what was chosen, by the option's own text, and
///   where the session went. The text is what survives a reordering; the route
///   is what the session actually did.
///
/// Fields that were not measured are absent rather than zero. A [`Resolution`]
/// carries a score where a score was observed, and a [`Resolution::Degraded`]
/// carries none at all, which is the difference between *the options were
/// compared and nothing was close* and *nothing was compared*.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct DecisionReceipt {
    /// Schema version; see [`RECEIPT_VERSION`].
    pub(super) version: u32,
    /// The session the decision was taken in.
    pub(super) session: SessionId,
    /// Digest of the flow document the session is running.
    pub(super) flow: String,
    /// The ask node the decision was taken at.
    pub(super) node: NodeId,
    /// Digest of the ordered candidate options the verdict was reached against.
    pub(super) options: String,
    /// The verdict, verbatim.
    pub(super) resolution: Resolution,
    /// The policy in force, and the model when one was consulted.
    pub(super) provenance: Provenance,
    /// The selected option's own text, when the verdict selected one.
    ///
    /// The text rather than the index: an index is a position in a list that
    /// can be reordered, and the text is the only thing that still names the
    /// choice once it is.
    pub(super) selected: Option<String>,
    /// The node the accepted transition moved to, when it moved.
    pub(super) route: Option<NodeId>,
}

impl DecisionReceipt {
    /// Returns the schema version.
    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    /// Returns the session identity.
    #[must_use]
    pub fn session(&self) -> &SessionId {
        &self.session
    }

    /// Returns the flow revision digest.
    #[must_use]
    pub fn flow(&self) -> &str {
        &self.flow
    }

    /// Returns the ask node the decision was taken at.
    #[must_use]
    pub fn node(&self) -> &str {
        &self.node
    }

    /// Returns the digest of the ordered candidate options.
    #[must_use]
    pub fn options(&self) -> &str {
        &self.options
    }

    /// Returns the verdict.
    #[must_use]
    pub fn resolution(&self) -> &Resolution {
        &self.resolution
    }

    /// Returns the policy and model behind the verdict.
    #[must_use]
    pub fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    /// Returns the selected option's text, when the verdict selected one.
    #[must_use]
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// Returns the node the accepted transition moved to, when it moved.
    #[must_use]
    pub fn route(&self) -> Option<&str> {
        self.route.as_deref()
    }
}

/// Where a journal accepted a decision receipt.
///
/// Reported by the sink rather than assumed by the session, because the two are
/// different facts and only the sink knows which one is true. A receipt held in
/// memory is evidence for the lifetime of the process; a receipt held durably
/// is evidence after it. A run that could not tell them apart would put the
/// second claim on the first record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReceiptAcceptance {
    /// Held only in this process, and gone when it ends.
    InMemory,
    /// Written to a sink that outlives the process.
    Durable,
}

/// The failure a journal reports when it cannot accept a receipt.
///
/// A named sink and its own description rather than a closed enum of causes:
/// the failure belongs to the sink, which owns its vocabulary — a full disk, a
/// refused connection, a closed file — and this crate cannot enumerate another
/// component's failures without inventing a taxonomy the sink would then have
/// to translate into. What is fixed here is that the failure crosses the seam
/// as a value the session must handle, rather than a diagnostic it may print
/// and continue past.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct JournalError {
    /// The sink that refused the receipt.
    pub(super) sink: String,
    /// The sink's own description of the failure.
    pub(super) cause: String,
}

impl JournalError {
    /// Records a sink's failure to accept a receipt.
    #[must_use]
    pub fn new(sink: impl Into<String>, cause: impl Into<String>) -> Self {
        Self {
            sink: sink.into(),
            cause: cause.into(),
        }
    }

    /// Returns the sink that refused.
    #[must_use]
    pub fn sink(&self) -> &str {
        &self.sink
    }

    /// Returns the sink's description of the failure.
    #[must_use]
    pub fn cause(&self) -> &str {
        &self.cause
    }
}

impl fmt::Display for JournalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "journal {} refused a decision receipt: {}",
            self.sink, self.cause
        )
    }
}

impl std::error::Error for JournalError {}

/// One receipt, and the way the journal accepted it.
///
/// The pair rather than a receipt carrying its own acceptance: the receipt is
/// what both sides hold, and the acceptance is the journal's answer about it.
/// Folding the answer into the record the journal was already given would mean
/// the two copies differ on the field that matters most.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct RecordedDecision {
    /// The receipt the journal accepted.
    pub(super) receipt: DecisionReceipt,
    /// Where the journal said it accepted it.
    pub(super) acceptance: ReceiptAcceptance,
}

impl RecordedDecision {
    /// The journal receipt this decision was written under.
    ///
    /// Borrowed, not cloned, so a caller cannot present a receipt that is not
    /// the one the journal accepted. A receipt is the durable half of a decision:
    /// it is what makes the decision survive a restart that loses this record.
    #[must_use]
    pub fn receipt(&self) -> &DecisionReceipt {
        &self.receipt
    }

    /// Returns where the journal accepted it.
    #[must_use]
    pub const fn acceptance(&self) -> ReceiptAcceptance {
        self.acceptance
    }
}

/// Journal seam for session path records.
pub trait Journal {
    /// Receive a path node, role label, and rendered text record.
    fn record(&mut self, path_node: &str, role: &str, text: &str);

    /// Accept one decision receipt, reporting where it was accepted.
    ///
    /// Fallible, and required rather than defaulted. [`Self::record`] cannot
    /// report persistence failure, and neither could a defaulted version of
    /// this method: a sink that has to do nothing to look successful is a sink
    /// that silently drops every receipt, and the session would report an
    /// accepted transition whose audit record does not exist. Requiring it
    /// makes every implementation state what it does with a receipt — including
    /// [`MemoryJournal`], which says `InMemory` in as many words.
    ///
    /// `Err` aborts the transition. A receipt is not a log line written beside
    /// the work; it is part of accepting the answer, so a session that cannot
    /// record the decision has not taken it.
    fn record_decision(
        &mut self,
        receipt: &DecisionReceipt,
    ) -> Result<ReceiptAcceptance, JournalError>;
}

/// One in-memory transcript record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct TranscriptEntry {
    /// Node id at which the record was produced.
    pub(super) path_node: String,
    /// Role label, such as `assistant` or `user`.
    pub(super) role: String,
    /// Rendered record text.
    pub(super) text: String,
}

impl TranscriptEntry {
    /// The node this transcript entry was recorded against.
    ///
    /// The *path* node rather than the current one: a session that loops back
    /// through a branch records entries against the node that was current when
    /// they happened, so a reader replaying the transcript follows the path the
    /// conversation actually took rather than the one it ended on.
    #[must_use]
    pub fn path_node(&self) -> &str {
        &self.path_node
    }

    /// Return the role label.
    #[must_use]
    pub fn role(&self) -> &str {
        &self.role
    }

    /// Return the rendered record text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Bounded-by-session in-memory journal implementation.
///
/// `PartialEq` but not `Eq`: the receipts it holds carry the scores a verdict
/// was reached on, and `f64` is not `Eq`. That is a property of the record
/// rather than a gap — `NaN` is not a score any tier produces, and `DecisionReceipt`
/// has no `Eq` for the same reason.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct MemoryJournal {
    /// Records in insertion order.
    pub(super) records: Vec<TranscriptEntry>,
    /// Decision receipts in insertion order.
    pub(super) receipts: Vec<DecisionReceipt>,
}

impl MemoryJournal {
    /// Construct an empty journal.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            records: Vec::new(),
            receipts: Vec::new(),
        }
    }

    /// Return records in insertion order.
    #[must_use]
    pub fn records(&self) -> &[TranscriptEntry] {
        &self.records
    }

    /// Return decision receipts in insertion order.
    #[must_use]
    pub fn receipts(&self) -> &[DecisionReceipt] {
        &self.receipts
    }

    /// Consume the journal and return its records.
    #[must_use]
    pub fn into_records(self) -> Vec<TranscriptEntry> {
        self.records
    }

    /// Consume the journal and return its decision receipts.
    #[must_use]
    pub fn into_receipts(self) -> Vec<DecisionReceipt> {
        self.receipts
    }
}

impl Journal for MemoryJournal {
    fn record(&mut self, path_node: &str, role: &str, text: &str) {
        self.records.push(TranscriptEntry {
            path_node: path_node.to_owned(),
            role: role.to_owned(),
            text: text.to_owned(),
        });
    }

    /// Accepts a receipt, and says so: `InMemory` is the honest answer for a
    /// sink whose whole storage is a `Vec` in this process. A caller that needs
    /// a durable record has to install a journal that says `Durable`, and can
    /// see from the receipt that this one did not.
    fn record_decision(
        &mut self,
        receipt: &DecisionReceipt,
    ) -> Result<ReceiptAcceptance, JournalError> {
        self.receipts.push(receipt.clone());
        Ok(ReceiptAcceptance::InMemory)
    }
}

/// Public identity of one in-memory session.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
#[non_exhaustive]
pub struct SessionId(String);

impl SessionId {
    /// Construct an id from its stable string representation.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Return the stable string representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for SessionId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for SessionId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}
