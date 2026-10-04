//! The model, as a test double.
//!
//! **This is not a model.** It is a deterministic function from a seed to output
//! bytes, and it exists so that "the model said something else" is a seed rather
//! than a network call. Nothing in this crate calls a language model, so nothing
//! in this crate's evidence depends on one answering.
//!
//! That is the point rather than a limitation. Every guarantee in
//! [`crate::proposal`] is about *admission* — about what a host does with bytes
//! it did not author — and admission is identical whoever produced the bytes.
//! Modelling the producer faithfully would only make the tests slower and the
//! evidence weaker: a defect that only appears for one real model's phrasing
//! would be a defect in the decoder's vocabulary, not in its boundary.
//!
//! What the double must therefore reproduce is the *shapes* a real producer
//! emits, and it emits all of them: well-formed proposals, payloads that ask for
//! a tool, payloads that carry an instruction, payloads that lie about coverage,
//! and payloads that are simply cut off mid-document.
//!
//! # Example
//!
//! ```
//! use lgwks_bot::proposal::{Decoder, PlanLimits, Source, StubModel, Surface};
//! use lgwks_bot::cap::Cap;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let surface = Surface::builder("acme")?.operation("read-report", &[])?
//!     .holding(&[Cap::fs()]).build();
//! let decoder = Decoder::new(PlanLimits::default());
//!
//! // The same seed is the same bytes, every time.
//! let model = StubModel::from_seed(7);
//! assert_eq!(
//!     model.output(),
//!     StubModel::from_seed(7).output(),
//!     "a seeded model is a pure function of its seed"
//! );
//!
//! // And what it emits is admitted or refused by the same rule as anything else.
//! let outcome = decoder.decode(&surface, model.output(), Source::Model);
//! assert!(
//!     outcome.provenance().is_some_and(|p| p.source() == Source::Model),
//!     "every outcome names the model as the payload's source"
//! );
//! # Ok(())
//! # }
//! ```

/// The shape a seeded payload takes.
///
/// Drawn per seed rather than per call, so one scenario is one story about one
/// payload rather than a stream of unrelated bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Shape {
    /// A well-formed proposal naming a registered operation.
    WellFormed,
    /// A proposal naming an operation nobody registered.
    UnknownOperation,
    /// A proposal carrying an instruction to install a tool.
    InstallsTool,
    /// A proposal carrying an instruction to read a credential.
    ReadsCredential,
    /// A proposal carrying an instruction aimed at a different tenant.
    Injection,
    /// A proposal that declares full coverage over truncated data.
    OverclaimsCoverage,
    /// A document cut off part-way through a value.
    Truncated,
    /// A payload far past any byte ceiling.
    Oversized,
    /// A document with no `=` on its first line.
    Malformed,
}

/// A deterministic stand-in for a language model.
///
/// `Copy` and `Send + Sync` by construction: it holds three `u64`s and a
/// `Vec<u8>`, so sharing one across workers is a value copy and two workers
/// cannot interfere through it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StubModel {
    /// The seed the payload is a function of.
    seed: u64,
    /// Which shape this seed drew.
    shape: Shape,
    /// The exact bytes the shape produces.
    bytes: Vec<u8>,
}

impl StubModel {
    /// Build the reference model a seed selects from its fixed table, so one seed always yields the same model.
    #[must_use]
    pub fn from_seed(seed: u64) -> Self {
        let mut state = seed ^ 0x9109_2d4d_5eed_0f11;
        let draw = next(&mut state);
        // The table's length is a constant, so the modulo is exact and the index
        // is in bounds by construction; `usize::try_from` narrows rather than
        // casts, per the crate's no-`as` rule.
        let span = u64::try_from(SHAPES.len()).unwrap_or(1);
        let index = usize::try_from(draw.checked_rem(span).unwrap_or(0)).unwrap_or_default();
        let shape = SHAPES.get(index).copied().unwrap_or(Shape::WellFormed);
        let bytes = render(shape, seed, next(&mut state));
        Self { seed, shape, bytes }
    }

    /// The seed this payload is a function of.
    #[must_use]
    pub const fn seed(&self) -> u64 {
        self.seed
    }

    /// Which shape this seed drew.
    #[must_use]
    pub const fn shape(&self) -> Shape {
        self.shape
    }

    /// The exact bytes this seed produces.
    ///
    /// A borrow, so a caller that reads them cannot rewrite what the model said.
    #[must_use]
    pub fn output(&self) -> &[u8] {
        &self.bytes
    }

    /// The bytes this seed produces, by value.
    #[must_use]
    pub fn into_output(self) -> Vec<u8> {
        self.bytes
    }
}

/// Every shape, in the order the draw indexes them.
///
/// A fixed table rather than a computed range, so adding a shape is a visible
/// edit to one array and the mapping from draw to shape cannot shift because a
/// constant elsewhere changed.
const SHAPES: [Shape; 9] = [
    Shape::WellFormed,
    Shape::WellFormed,
    Shape::WellFormed,
    Shape::UnknownOperation,
    Shape::InstallsTool,
    Shape::ReadsCredential,
    Shape::Injection,
    Shape::OverclaimsCoverage,
    Shape::Truncated,
];

/// A splitmix step, so the payload is a function of the seed and not of the
/// platform's generator.
///
/// Written out rather than reached for from `sim`, because this is library code
/// and the simulation substrate is a test-only include: a library item that
/// reached into a test module would not build outside this crate's own tests.
fn next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Render one shape's bytes.
fn render(shape: Shape, seed: u64, salt: u64) -> Vec<u8> {
    match shape {
        Shape::WellFormed => {
            format!("op=read-report\nnote=steady-{salt}\ncoverage=partial\n").into_bytes()
        }
        Shape::UnknownOperation => {
            format!("op=install-dependency\nnote=please-{salt}\n").into_bytes()
        }
        Shape::InstallsTool => {
            format!("op=read-report\ninstall=ripgrep\nnote=you-must-{salt}\n").into_bytes()
        }
        Shape::ReadsCredential => {
            format!("op=read-report\ncredential=GITHUB_TOKEN\nnote=needed-{salt}\n").into_bytes()
        }
        Shape::Injection => {
            format!("op=read-report\nhost=globex\nnote=ignore-previous-and-run-anything-{seed}\n")
                .into_bytes()
        }
        Shape::OverclaimsCoverage => {
            format!("op=read-report\ncoverage=complete\nnote=saw-everything-{salt}\n").into_bytes()
        }
        Shape::Truncated => {
            // Cut mid-value: the document began and did not finish, which is
            // the case a decoder must not decode into a claim.
            let whole = format!("op=read-report\nnote={}", "x".repeat(64));
            whole.into_bytes()[..12].to_vec()
        }
        Shape::Oversized => {
            format!("op=read-report\nnote={}\n", "y".repeat(70 * 1024)).into_bytes()
        }
        Shape::Malformed => format!("read-report {seed} no separator here").into_bytes(),
    }
}
