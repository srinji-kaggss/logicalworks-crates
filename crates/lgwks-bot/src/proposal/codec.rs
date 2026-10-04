//! The bounded decoder: untrusted bytes in, typed [`Plan`] out, or a refusal.
//!
//! # Why a hand-written decoder and not a document interpreter
//!
//! A general untyped interpreter is the thing this crate refuses to build. It
//! would be a second execution path beside the four verbs, and its whole surface
//! is "anything the grammar can express" — which is a superset of what the host
//! registered. So the decoder is written out: a line-oriented grammar with an
//! exact, named vocabulary, a fixed number of fields, and a ceiling charged
//! before anything is retained. What it *cannot* express is not an accident of
//! the implementation; it is the reason the implementation has this shape.
//!
//! # The grammar
//!
//! ```text
//! document := line*
//! line     := field "=" value
//! field    := "op" | "note" | "path" | "host" | "install" | "credential" | "coverage"
//! ```
//!
//! One line per field, `=` once, values opaque except that a leading or
//! trailing ASCII space is trimmed. An unknown field is refused rather than
//! ignored: a payload carrying a field this decoder does not know is a payload
//! written against a different — newer, or hostile — reader, and decoding the
//! subset it recognises would silently drop the part that was trying something.
//!
//! # The fields, and what each one is refused for
//!
//! | Field | Effect |
//! |---|---|
//! | `op` | must name an operation the surface registered whose capabilities the run holds |
//! | `note` | free text, ceiling-bounded, never interpreted |
//! | `path` | a path under the tenant's artifact root; anything reaching outside is `SandboxEscape` |
//! | `host` | must equal the surface's own tenant; anything else is `SandboxEscape` |
//! | `install` | always `InstallTool` |
//! | `credential` | always `CredentialRead` |
//! | `coverage` | `complete` is only ever a *claim*; the admitted plan carries [`Coverage::Partial`] unless a completion claim admitted it |
//!
//! The last three rows are the reason this file exists. `install` and
//! `credential` are *recognised* so that their refusal names what was asked for;
//! a decoder that had never heard of them would report `Malformed` and a reader
//! counting refusals would see a broken document rather than an attempt to widen
//! authority.

use std::fmt;

use super::{Coverage, MAX_FIELD_NAME_BYTES, Outcome, Provenance, Refusal, Source, Surface};

/// The declared shape of one plan document.
///
/// Every ceiling is named, defaulted, and readable, so a caller knows the bound
/// its refusals are measured against instead of inferring it from a number in a
/// message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PlanLimits {
    /// The most bytes one payload may occupy.
    pub max_bytes: usize,
    /// The most bytes one field value may occupy.
    pub max_field_bytes: usize,
    /// The most fields one document may carry.
    pub max_fields: u32,
}

impl Default for PlanLimits {
    /// [`PlanLimits::default_bytes`], [`PlanLimits::default_field_bytes`], and
    /// eight fields.
    ///
    /// Eight, not a hundred: a proposal that names four operations and one note
    /// is a proposal; one that names a hundred fields is a document, and this
    /// crate has no interpreter for documents.
    fn default() -> Self {
        Self {
            max_bytes: Self::default_bytes(),
            max_field_bytes: Self::default_field_bytes(),
            max_fields: 8,
        }
    }
}

impl PlanLimits {
    /// Limits of exactly `max_bytes`, `max_field_bytes` and `max_fields`.
    ///
    /// The constructor rather than a struct literal, because the type is
    /// `#[non_exhaustive]`: a caller outside this crate names its ceilings here
    /// and gets a value that a future field does not silently default.
    #[must_use]
    pub const fn new(max_bytes: usize, max_field_bytes: usize, max_fields: u32) -> Self {
        Self {
            max_bytes,
            max_field_bytes,
            max_fields,
        }
    }

    /// These limits with the payload ceiling replaced.
    #[must_use]
    pub const fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// These limits with the value ceiling replaced.
    #[must_use]
    pub const fn with_max_field_bytes(mut self, max_field_bytes: usize) -> Self {
        self.max_field_bytes = max_field_bytes;
        self
    }

    /// These limits with the field-count ceiling replaced.
    #[must_use]
    pub const fn with_max_fields(mut self, max_fields: u32) -> Self {
        self.max_fields = max_fields;
        self
    }

    /// The default payload ceiling: [`super::MAX_PLAN_BYTES`].
    #[must_use]
    pub const fn default_bytes() -> usize {
        super::MAX_PLAN_BYTES
    }

    /// The default value ceiling: [`super::MAX_FIELD_BYTES`].
    #[must_use]
    pub const fn default_field_bytes() -> usize {
        super::MAX_FIELD_BYTES
    }
}

/// What a payload declared it wanted, as raw strings.
///
/// The pre-authorization view: every value is still a string and no capability
/// has been checked. [`Decoder::decode`] turns one of these into a
/// [`Outcome::Admitted`] plan, and nothing else in this crate can, which is what
/// makes the authorization unskippable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Declared {
    /// The `op` values, in the order they appeared.
    ops: Vec<String>,
    /// The `note` values, in the order they appeared.
    notes: Vec<String>,
    /// The `path` values, in the order they appeared.
    paths: Vec<String>,
    /// The `coverage` value, if any.
    coverage: Option<String>,
}

impl Declared {
    /// The operations the payload named.
    #[must_use]
    pub fn ops(&self) -> &[String] {
        &self.ops
    }

    /// The notes the payload carried.
    #[must_use]
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// The paths the payload named.
    #[must_use]
    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    /// The coverage claim the payload made.
    ///
    /// A claim, not a result: [`Coverage::from_claim`] is what turns it into
    /// the conservative value a plan carries.
    #[must_use]
    pub fn coverage(&self) -> Option<&str> {
        self.coverage.as_deref()
    }

    /// Whether the payload named nothing to do.
    ///
    /// A note alone is not work. A payload that only commented on the task is
    /// refused as [`Refusal::Empty`], because admitting it would let a model
    /// that produced nothing consume a repair attempt.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty() && self.paths.is_empty()
    }
}

/// One value a payload asked for: a registered operation, applied to an optional
/// scoped input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    /// The operation this value is for.
    operation: String,
    /// The path this value applies to, when the payload named one.
    path: Option<String>,
}

impl Wanted {
    /// The name of the operation this proposal asks to run, as it must appear in the surface's registry.
    #[must_use]
    pub fn operation(&self) -> &str {
        &self.operation
    }

    /// The path this value applies to.
    #[must_use]
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }
}

/// A payload that passed every check and became work.
///
/// Constructible only by [`Decoder::decode`], so an `Admitted` outcome cannot
/// exist without an authorization having run: the type has no public
/// constructor and no public field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// What the payload asked for, one entry per `op` line.
    wanted: Vec<Wanted>,
    /// The notes it carried, never interpreted.
    notes: Vec<String>,
    /// The coverage the payload may claim, and no more than `partial`.
    coverage: Coverage,
    /// The digest of the exact bytes this plan came from.
    provenance: Provenance,
}

impl Plan {
    /// What the payload asked for, in the order it named them.
    #[must_use]
    pub fn wanted(&self) -> &[Wanted] {
        &self.wanted
    }

    /// The notes the payload carried.
    #[must_use]
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// The coverage this plan may report.
    ///
    /// A plan never carries [`Coverage::Complete`]. Completion is a claim with
    /// evidence behind it, established by [`crate::proposal::Completion::admit`]
    /// and not by a plan, so a decoder cannot be talked into a complete
    /// observation however many `coverage=complete` lines it is given.
    #[must_use]
    pub const fn coverage(&self) -> Coverage {
        self.coverage
    }

    /// Where these bytes came from.
    #[must_use]
    pub const fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    /// The plan a repair ledger hands back while a failure may still be retried.
    ///
    /// Crate-private because it is not a proposal: it names no operation and
    /// asserts no coverage, and a caller that could build one would be able to
    /// manufacture an `Admitted` outcome without a decoder having run. The
    /// ledger is the only caller, and it uses this to keep the two halves of the
    /// API — "the run may repair" and "the payload became work" — from needing a
    /// third outcome arm.
    pub(crate) fn for_repair(fingerprint: &str, provenance: Provenance) -> Self {
        Self {
            wanted: Vec::new(),
            notes: vec![format!("retry: {fingerprint}")],
            coverage: Coverage::Partial {
                covered: 0,
                asked: 1,
            },
            provenance,
        }
    }
}

impl fmt::Display for Plan {
    /// The wanted operations, then the coverage label.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for wanted in &self.wanted {
            match wanted.path() {
                Some(path) => write!(formatter, "{} {} ", wanted.operation(), path)?,
                None => write!(formatter, "{} ", wanted.operation())?,
            }
        }
        write!(formatter, "[{}]", self.coverage.label())
    }
}

/// The digest of `payload` as a plan.
///
/// Framed under its own label, so a plan digest and an
/// [`ArtifactKey`](super::ArtifactKey) digest over the same bytes differ. A
/// caller that wants to name "this exact payload" — in a report, a log, or a
/// checkpoint — hashes it here rather than reaching for a bare
/// [`lgwks_std::hash::blake3`], so one payload has one identity in this crate
/// rather than one per call site.
#[must_use]
pub fn payload_digest(payload: &[u8]) -> lgwks_std::hash::Digest {
    super::framed_digest("lgwks-bot/proposal/plan", payload)
}

/// The bounded decoder for untrusted payloads.
///
/// Holds only ceilings, so it is cheap to share and carries no tenant, no
/// registry and no state — nothing a payload could reach by influencing a
/// previous decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decoder {
    /// The ceilings every decode is measured against.
    limits: PlanLimits,
}

impl Decoder {
    /// A decoder measuring against `limits`.
    #[must_use]
    pub const fn new(limits: PlanLimits) -> Self {
        Self { limits }
    }

    /// The ceilings every decode is measured against.
    #[must_use]
    pub const fn limits(&self) -> PlanLimits {
        self.limits
    }

    /// Decode `payload` as a proposal for `surface`, recording `source` as where
    /// the bytes came from.
    ///
    /// The one door untrusted bytes enter through. It charges the payload's size
    /// before reading a byte, refuses an attempt to widen authority before it can
    /// become a plan, and returns the provenance of the exact bytes on every path
    /// so no outcome is unattributable.
    ///
    /// A refused payload is refused *whole*: no partial plan is returned beside
    /// the refusal, because a caller that got both would have work it could
    /// execute.
    #[must_use]
    pub fn decode(&self, surface: &Surface, payload: &[u8], source: Source) -> Outcome {
        let provenance = Provenance::of(source, surface.tenant(), payload);
        if let Err(refusal) = self.check_size(payload) {
            return Outcome::refused(provenance, refusal);
        }
        match self.read(surface, payload) {
            Err(refusal) => Outcome::refused(provenance, refusal),
            Ok(declared) => self.authorize(surface, &declared, provenance),
        }
    }

    /// Charge the payload against the byte ceiling before decoding it.
    fn check_size(&self, payload: &[u8]) -> Result<(), Refusal> {
        if payload.len() > self.limits.max_bytes {
            let refusal = Err(Refusal::Oversized {
                got: payload.len(),
                limit: self.limits.max_bytes,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "check_size: returning an error to the caller");
            return refusal;
        }
        Ok(())
    }

    /// Authorize `declared` against `surface`, producing the plan it admits to.
    fn authorize(&self, surface: &Surface, declared: &Declared, provenance: Provenance) -> Outcome {
        match self.plan(surface, declared, provenance.clone()) {
            Ok(plan) => Outcome::Admitted { plan, provenance },
            Err(refusal) => Outcome::refused(provenance, refusal),
        }
    }

    /// Build the plan `declared` admits to, or the refusal that stops it.
    fn plan(
        &self,
        surface: &Surface,
        declared: &Declared,
        provenance: Provenance,
    ) -> Result<Plan, Refusal> {
        if declared.is_empty() {
            let refusal = Err(Refusal::Empty);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "plan: returning an error to the caller");
            return refusal;
        }
        for path in declared.paths() {
            refuse_traversal(path)?;
        }
        let mut wanted = Vec::with_capacity(declared.ops().len());
        for op in declared.ops() {
            // `authorize` is the whole of the privilege check: the name must be
            // registered and every capability it needs must already be held. It
            // cannot be skipped from here, and there is no other path that
            // builds a `Wanted`.
            surface.authorize(op)?;
            wanted.push(Wanted {
                operation: op.clone(),
                path: declared.paths().first().cloned(),
            });
        }
        Ok(Plan {
            wanted,
            notes: declared.notes().to_vec(),
            coverage: Coverage::from_claim(declared.coverage()),
            provenance,
        })
    }

    /// Read `payload` as a document under `surface`, or say where it stopped.
    fn read(&self, surface: &Surface, payload: &[u8]) -> Result<Declared, Refusal> {
        Reader {
            bytes: payload,
            cursor: 0,
            limits: self.limits,
            surface: Some(surface),
        }
        .document()
    }
}

/// Refuse a path that leaves the tenant's artifact root.
///
/// Three spellings reach outside: an absolute path, a Windows-style drive or
/// UNC prefix, and a `..` segment that walks up. All three are refusals with the
/// offending value attributed, because "the sandbox refused" without saying what
/// it refused is not observable.
fn refuse_traversal(path: &str) -> Result<(), Refusal> {
    if path.starts_with('/') || path.starts_with('\\') || path.contains(':') {
        let refusal = Err(Refusal::escape("path", path));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "refuse_traversal: returning an error to the caller");
        return refusal;
    }
    if path.split('/').any(|segment| segment == "..") {
        let refusal = Err(Refusal::escape("path", path));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "refuse_traversal: returning an error to the caller");
        return refusal;
    }
    Ok(())
}

// ── The reader ───────────────────────────────────────────────────────────────

/// One byte-at-a-time reader over the payload.
///
/// A cursor and a window rather than `split`, because three of the four refusal
/// kinds here are about *position*: an oversize value is refused before it is
/// retained, and a malformed document names the offset it stopped at. A
/// `lines()` iterator would have materialized the oversize value first.
struct Reader<'a> {
    /// The payload.
    bytes: &'a [u8],
    /// How far it has read.
    cursor: usize,
    /// The ceilings it charges against.
    limits: PlanLimits,
    /// The surface whose tenant a `host` field is measured against, installed by
    /// [`Decoder::decode`] before the first line is read.
    surface: Option<&'a Surface>,
}

impl<'a> Reader<'a> {
    /// Read the whole document.
    fn document(&mut self) -> Result<Declared, Refusal> {
        let mut declared = Declared::default();
        let mut fields = 0_u32;
        loop {
            self.skip_newlines();
            if self.cursor >= self.bytes.len() {
                return Ok(declared);
            }
            let start = self.cursor;
            let line = self.line()?;
            if line.is_empty() {
                // A blank line is not a field. It is allowed rather than refused
                // because a trailing newline is the ordinary way a writer ends a
                // document, and refusing it would make every well-formed payload
                // one byte from a malformed one.
                continue;
            }
            fields = fields.saturating_add(1);
            if fields > self.limits.max_fields {
                let refusal = Err(Refusal::Limit {
                    what: "the number of fields in a plan",
                    got: u64::from(fields),
                    limit: u64::from(self.limits.max_fields),
                });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "document: returning an error to the caller");
                return refusal;
            }
            self.field(&mut declared, line, start)?;
        }
    }

    /// Read one line's bytes, without its terminator.
    fn line(&mut self) -> Result<&'a [u8], Refusal> {
        let start = self.cursor;
        while self.cursor < self.bytes.len() {
            if self.bytes[self.cursor] == b'\n' {
                let line = &self.bytes[start..self.cursor];
                self.cursor = self.cursor.saturating_add(1);
                return Ok(trim_cr(line));
            }
            self.cursor = self.cursor.saturating_add(1);
            let taken = self.cursor.saturating_sub(start);
            if taken > self.line_ceiling() {
                // A line this long cannot be within the value ceiling whatever it
                // says, so it is refused before the value is sliced out of it.
                let refusal = Err(Refusal::Limit {
                    what: "the length of a line in a plan",
                    got: u64::try_from(taken).unwrap_or(u64::MAX),
                    limit: u64::try_from(self.line_ceiling()).unwrap_or(u64::MAX),
                });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "line: returning an error to the caller");
                return refusal;
            }
        }
        // No terminator: the document ended without a final newline. That is
        // still a complete document, because every field before this one was
        // terminated and this is the last.
        Ok(trim_cr(&self.bytes[start..self.cursor]))
    }

    /// The longest a line may be and still possibly be within every ceiling:
    /// the value ceiling plus the longest field name and the `=` between them.
    fn line_ceiling(&self) -> usize {
        self.limits
            .max_field_bytes
            .saturating_add(MAX_FIELD_NAME_BYTES)
            .saturating_add(1)
    }

    /// Charge and store one field's value.
    fn field(&mut self, declared: &mut Declared, line: &[u8], start: usize) -> Result<(), Refusal> {
        let Some(split) = line.iter().position(|byte| *byte == b'=') else {
            let refusal = Err(Refusal::Malformed {
                cause: "a line with no `=` separator",
                at: start,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "field: returning an error to the caller");
            return refusal;
        };
        let raw_name = &line[..split];
        let raw_value = &line[split.saturating_add(1)..];
        self.check_len(
            raw_name.len(),
            MAX_FIELD_NAME_BYTES,
            "a field name in a plan",
            start,
        )?;
        self.check_len(
            raw_value.len(),
            self.limits.max_field_bytes,
            "a field value in a plan",
            start,
        )?;
        let name = decode_text(raw_name);
        let value = decode_text(raw_value);
        self.store(declared, &name, value, start)
    }

    /// Refuse a slice longer than `limit`, naming the offset it started at.
    fn check_len(
        &self,
        len: usize,
        limit: usize,
        what: &'static str,
        at: usize,
    ) -> Result<(), Refusal> {
        if len > limit {
            let refusal = Err(Refusal::Malformed { cause: what, at });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "check_len: returning an error to the caller");
            return refusal;
        }
        Ok(())
    }

    /// Put one field into `declared`, or refuse what it asked for.
    fn store(
        &mut self,
        declared: &mut Declared,
        name: &str,
        value: String,
        start: usize,
    ) -> Result<(), Refusal> {
        match name {
            "op" => declared.ops.push(value),
            "note" => declared.notes.push(value),
            "path" => declared.paths.push(value),
            "coverage" => declared.coverage = Some(value),
            // Named so the refusal is about the capability rather than the
            // grammar: the payload asked for a tool, and this decoder is what
            // says no.
            "install" => {
                let refusal = Err(Refusal::install_tool(&value));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "store: returning an error to the caller");
                return refusal;
            }
            "credential" => {
                let refusal = Err(Refusal::credential_read(&value));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "store: returning an error to the caller");
                return refusal;
            }
            // The one field whose legality depends on the surface rather than on
            // the grammar: `host` naming this run's own tenant is the same
            // tenant, and naming any other is leaving the run's boundary.
            "host" => return self.refuse_foreign_host(&value, start),
            _ => {
                let refusal = Err(Refusal::Malformed {
                    cause: "a field this decoder does not recognise",
                    at: start,
                });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "store: returning an error to the caller");
                return refusal;
            }
        }
        Ok(())
    }

    /// Refuse a `host` field that does not name this run's own tenant.
    fn refuse_foreign_host(&self, value: &str, at: usize) -> Result<(), Refusal> {
        let Some(surface) = self.surface else {
            let refusal = Err(Refusal::Malformed {
                cause: "a `host` field outside a run surface",
                at,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "refuse_foreign_host: returning an error to the caller");
            return refusal;
        };
        if value == surface.tenant() {
            Ok(())
        } else {
            Err(Refusal::escape("host", value))
        }
    }

    /// Skip blank lines so a document with generous spacing is not a refusal.
    fn skip_newlines(&mut self) {
        while self.cursor < self.bytes.len() && self.bytes[self.cursor] == b'\n' {
            self.cursor = self.cursor.saturating_add(1);
        }
    }
}

/// Drop one carriage return from a line that ended `\r\n`.
fn trim_cr(line: &[u8]) -> &[u8] {
    match line.split_last() {
        Some((&b'\r', rest)) => rest,
        Some(_) | None => line,
    }
}

/// Trim ASCII spaces from both ends of a field's name or value.
///
/// Spaces and not `trim()`, because `trim` also removes control characters, and
/// a value carrying a newline is a payload trying to be two documents.
fn trim(value: &[u8]) -> &[u8] {
    let start = value.iter().take_while(|byte| **byte == b' ').count();
    let end = value.iter().rev().take_while(|byte| **byte == b' ').count();
    // `start + end` cannot exceed the slice's length: both count disjoint
    // positions, so the subtraction is only reached when every byte is a space,
    // and then it is `len`.
    &value[start..value.len().saturating_sub(end)]
}

/// Read a field's bytes as text, replacing anything that is not UTF-8.
///
/// Lossy on purpose and lossy in a specific way: a payload is *bytes*, and
/// refusing a document because one value is not UTF-8 would let whoever wrote it
/// decide whether it is read at all. The replacement character keeps the value
/// the same length and the same position, so an oversize value is still charged
/// against the ceiling and an escape attempt is still refused by whatever it
/// wrote.
fn decode_text(raw: &[u8]) -> String {
    String::from_utf8_lossy(trim(raw)).into_owned()
}
