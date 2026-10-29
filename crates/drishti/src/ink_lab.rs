//! The ink lab: Logical Works' liquid ink motion engine, in Rust.
//!
//! The brand is "flat build, liquid motion": ink is drawn as many soft shapes,
//! fused by a "goo" filter (blur plus an alpha threshold), and every effect
//! settles onto the crisp static mark. This module is the deterministic
//! reference core behind the GPU runtime in [`crate::runtime`]. It emits frame
//! geometry and filter parameters; rasterization and the goo pass are the
//! residency layer's job.
//!
//! What is preserved from the JavaScript specification (`lw-ink.js`, S22):
//!
//! * the eleven effects — mark, text, wipe, drops, wobble, loader, pour,
//!   cursor, ripple, count, reveal;
//! * the adaptive quality tiers: auto drops to lite above ~26 ms per frame and
//!   recovers below ~17.5 ms after a hold;
//! * off-screen hosts stop accumulating, priority hosts run every frame, and
//!   other hosts share a stride under load;
//! * `prefers-reduced-motion` resolves every effect to its finished frame; and
//! * every effect ends in a crisp static state — nothing ships mid-liquid.
//!
//! What is deliberately different: time and randomness are injected, so one
//! seed and one input trace replay to the same frame digest. The JavaScript
//! engine reads the wall clock and `Math.random`.
//!
//! ```text
//! engine.step(dt) -> Result<[Render { host, frame }], InkError>
//! frame = shapes + alpha + blur + displacement (+ a count label)
//! ```

use crate::design::Rgb;

mod engine;

pub use engine::InkEngine;

/// Upper bound on shapes produced for a single host in one frame.
///
/// The mark's twenty-four beams are the largest fixed effect; this cap is
/// enforcement, not a suggestion, so a hostile host cannot grow the frame
/// without bound (requirement R08).
pub const MAX_SHAPES: usize = 512;

/// Upper bound on cursor beads, whatever [`Options::beads`] requests.
///
/// The bead chain is persistent per-host state, so an unbounded request would
/// grow memory without limit (R08); the brand default is eight.
pub const MAX_BEADS: usize = 64;

/// Frame-time EMA above which auto quality drops to lite, in milliseconds.
///
/// Matches the JavaScript engine's 26 ms threshold.
pub const LITE_ENTER_MS: f64 = 26.0;

/// Frame-time EMA below which auto quality recovers to full, in milliseconds.
///
/// Matches the JavaScript engine's 17.5 ms threshold.
pub const LITE_LEAVE_MS: f64 = 17.5;

/// The eleven ink effects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Effect {
    /// Beams squirt outward as ink, fuse, then resolve to crisp polygons.
    Mark,
    /// Letters condense out of a blurred, turbulent pool. Display type only.
    Text,
    /// A liquid level rises, holds and drains: the page transition.
    Wipe,
    /// A drop swells, falls and splashes into a living puddle.
    Drops,
    /// An idle liquid edge that leans toward the pointer.
    Wobble,
    /// Three drops orbit, fuse and part. For waits above 400 ms.
    Loader,
    /// A progress bar of liquid that leans at the meniscus.
    Pour,
    /// A chain of ink beads follows the pointer and wanders when idle.
    Cursor,
    /// A click splashes an ink bloom that spreads and thins out.
    Ripple,
    /// A number counts up in tabular figures.
    Count,
    /// A liquid level drains to uncover a surface once it scrolls into view.
    Reveal,
}

impl Effect {
    /// All eleven effects, in brand order.
    #[must_use]
    pub const fn all() -> [Self; 11] {
        [
            Self::Mark,
            Self::Text,
            Self::Wipe,
            Self::Drops,
            Self::Wobble,
            Self::Loader,
            Self::Pour,
            Self::Cursor,
            Self::Ripple,
            Self::Count,
            Self::Reveal,
        ]
    }

    /// The `data-ink` name from the brand specification.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Mark => "mark",
            Self::Text => "text",
            Self::Wipe => "wipe",
            Self::Drops => "drops",
            Self::Wobble => "wobble",
            Self::Loader => "loader",
            Self::Pour => "pour",
            Self::Cursor => "cursor",
            Self::Ripple => "ripple",
            Self::Count => "count",
            Self::Reveal => "reveal",
        }
    }
}

/// A geometry primitive the residency layer rasterizes.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum Shape {
    /// A soft circle, the ink engine's elementary blob.
    Circle {
        /// Centre x in CSS pixels.
        centre_x: f64,
        /// Centre y in CSS pixels.
        centre_y: f64,
        /// Radius in CSS pixels.
        radius: f64,
    },
    /// A stretched falling drop.
    Ellipse {
        /// Centre x.
        centre_x: f64,
        /// Centre y.
        centre_y: f64,
        /// Horizontal radius.
        radius_x: f64,
        /// Vertical radius.
        radius_y: f64,
    },
    /// A four-point beam polygon (the crisp mark).
    Quad {
        /// The four corners, clockwise from the narrow end.
        points: [[f64; 2]; 4],
    },
    /// An axis-aligned rectangle (a liquid body or fill).
    Rect {
        /// Left edge.
        left: f64,
        /// Top edge.
        top: f64,
        /// Right edge.
        right: f64,
        /// Bottom edge.
        bottom: f64,
    },
}

impl Shape {
    /// Mix this shape's geometry into a digest for replay comparison.
    fn mix(self, digest: &mut u64) {
        let mut mix = |value: f64| {
            *digest = (*digest ^ value.to_bits()).wrapping_mul(0x0000_0100_0000_01b3);
        };
        match self {
            Self::Circle {
                centre_x,
                centre_y,
                radius,
            } => {
                mix(1.0);
                mix(centre_x);
                mix(centre_y);
                mix(radius);
            }
            Self::Ellipse {
                centre_x,
                centre_y,
                radius_x,
                radius_y,
            } => {
                mix(2.0);
                mix(centre_x);
                mix(centre_y);
                mix(radius_x);
                mix(radius_y);
            }
            Self::Quad { points } => {
                mix(3.0);
                for point in points {
                    mix(point[0]);
                    mix(point[1]);
                }
            }
            Self::Rect {
                left,
                top,
                right,
                bottom,
            } => {
                mix(4.0);
                mix(left);
                mix(top);
                mix(right);
                mix(bottom);
            }
        }
    }
}

/// One host's frame: shapes plus the goo-filter parameters.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Frame {
    /// Soft shapes to rasterize, fused by the goo filter.
    shapes: Vec<Shape>,
    /// Global layer opacity in `[0, 1]`.
    pub alpha: f64,
    /// Gaussian blur standard deviation for the goo threshold, in pixels.
    pub blur: f64,
    /// Turbulence displacement scale, in pixels. Zero disables displacement.
    pub displacement: f64,
    /// Visible text, for the count effect; `None` for geometric effects.
    label: Option<String>,
}

impl Frame {
    /// An empty resolved frame.
    #[must_use]
    fn resolved() -> Self {
        Self {
            shapes: Vec::new(),
            alpha: 1.0,
            blur: 0.0,
            displacement: 0.0,
            label: None,
        }
    }

    /// The soft shapes to rasterize, as a bounded read-only slice.
    #[must_use]
    pub fn shapes(&self) -> &[Shape] {
        &self.shapes
    }

    /// The count effect's visible text, if any.
    #[must_use]
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// A deterministic digest of the frame for replay and trace hashing.
    #[must_use]
    pub fn digest(&self) -> u64 {
        let mut digest: u64 = 0xcbf2_9ce4_8422_2325;
        for shape in &self.shapes {
            shape.mix(&mut digest);
        }
        for value in [self.alpha, self.blur, self.displacement] {
            digest = (digest ^ value.to_bits()).wrapping_mul(0x0000_0100_0000_01b3);
        }
        if let Some(label) = self.label.as_deref() {
            for byte in label.bytes() {
                digest = (digest ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        digest
    }
}

/// A refused engine call. Malformed input is refused before it touches any
/// state, so a refused call never produces non-finite geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum InkError {
    /// A frame delta that is negative, NaN or infinite.
    InvalidDelta,
    /// A pointer or click coordinate that is not finite.
    InvalidInput,
    /// An option outside its declared domain; names the field.
    InvalidOption(&'static str),
}

impl std::fmt::Display for InkError {
    /// Name what was refused.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::InvalidDelta => {
                formatter.write_str("frame delta must be finite and non-negative")
            }
            Self::InvalidInput => {
                formatter.write_str("pointer and click coordinates must be finite")
            }
            Self::InvalidOption(field) => {
                write!(formatter, "option `{field}` is outside its domain")
            }
        }
    }
}

impl std::error::Error for InkError {}

/// Longest accepted duration, hold, stagger or auto-splash option: one hour.
pub const MAX_OPTION_MS: f64 = 3_600_000.0;

/// Largest accepted size, blur or displacement option, in pixels.
pub const MAX_OPTION_PIXELS: f64 = 10_000.0;

/// Largest accepted wobble amount or hover option.
pub const MAX_OPTION_AMOUNT: f64 = 1_000.0;

/// Largest accepted count magnitude; every such value is exact in `f64`'s
/// integer range and formats without exponent notation.
pub const MAX_COUNT_MAGNITUDE: f64 = 1.0e15;

/// Most decimal places a count may show.
pub const MAX_COUNT_DECIMALS: usize = 12;

/// Largest canvas axis, in CSS pixels.
pub const MAX_CANVAS: f64 = 16_384.0;

/// Check an optional value against `[low, high]`, refusing NaN and infinity.
fn within(value: Option<f64>, low: f64, high: f64, field: &'static str) -> Result<(), InkError> {
    match value {
        Some(number) if !(number.is_finite() && number >= low && number <= high) => {
            Err(InkError::InvalidOption(field))
        }
        Some(_) | None => Ok(()),
    }
}

/// Pointer and click input for one host on one frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Input {
    /// Pointer position in host-local pixels, if the pointer is inside.
    pub pointer: Option<(f64, f64)>,
    /// A completed click or tap in host-local pixels.
    pub click: Option<(f64, f64)>,
}

/// Quality mode, mirroring the JavaScript `LWInk.quality` API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Quality {
    /// Choose lite or full from the measured frame-time EMA.
    Auto,
    /// Force the reduced-work tier.
    Lite,
    /// Force full fidelity.
    Full,
}

/// A host's canvas size in CSS pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct Canvas {
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

impl Canvas {
    /// Build a canvas, clamping both axes to `[4, MAX_CANVAS]` pixels.
    ///
    /// A NaN axis clamps to the four-pixel minimum, so a canvas can never
    /// carry non-finite geometry into a frame.
    #[must_use]
    pub fn new(width: f64, height: f64) -> Self {
        Self {
            width: clamp_axis(width),
            height: clamp_axis(height),
        }
    }
}

/// Clamp one canvas axis to `[4, MAX_CANVAS]`; NaN becomes the minimum.
fn clamp_axis(value: f64) -> f64 {
    if value.is_nan() {
        4.0
    } else {
        value.clamp(4.0, MAX_CANVAS)
    }
}

/// Per-effect option overrides. Unset fields take the effect's brand default.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct Options {
    /// Mark ink colour, if the host wants to override the palette role.
    pub ink: Option<Rgb>,
    /// Accent colour for the leading beam or signal.
    pub accent: Option<Rgb>,
    /// Main duration in milliseconds.
    pub duration_ms: Option<f64>,
    /// How long a finished mark or wiped surface holds, in milliseconds.
    pub hold_ms: Option<f64>,
    /// Loop the effect instead of resolving.
    pub loop_forever: Option<bool>,
    /// Per-element stagger in milliseconds.
    pub stagger_ms: Option<f64>,
    /// Text blur in pixels; `None` derives it from the font size.
    pub blur: Option<f64>,
    /// Text displacement in pixels; `None` derives it from the font size.
    pub displacement: Option<f64>,
    /// Liquid fill in `[0, 1]`, for wipe, pour and reveal.
    pub level: Option<f64>,
    /// Bead count for the cursor effect.
    pub beads: Option<usize>,
    /// Idle amount for the wobble effect.
    pub amount: Option<f64>,
    /// Hover amount for the wobble effect.
    pub hover: Option<f64>,
    /// Drop size for the drops and loader effects.
    pub size: Option<f64>,
    /// Spout position for the drops effect, in `[0, 1]`.
    pub spout: Option<f64>,
    /// Automatic splash interval for ripple, in milliseconds; zero disables.
    pub auto_ms: Option<f64>,
    /// Count target value.
    pub count_to: Option<f64>,
    /// Count start value.
    pub count_from: Option<f64>,
    /// Count decimal places.
    pub count_decimals: Option<usize>,
    /// Count prefix.
    pub count_prefix: Option<&'static str>,
    /// Count suffix.
    pub count_suffix: Option<&'static str>,
}

impl Options {
    /// All defaults: the effect supplies the brand values.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ink: None,
            accent: None,
            duration_ms: None,
            hold_ms: None,
            loop_forever: None,
            stagger_ms: None,
            blur: None,
            displacement: None,
            level: None,
            beads: None,
            amount: None,
            hover: None,
            size: None,
            spout: None,
            auto_ms: None,
            count_to: None,
            count_from: None,
            count_decimals: None,
            count_prefix: None,
            count_suffix: None,
        }
    }
}

impl Options {
    /// Check every set field against its declared domain.
    ///
    /// Durations are `[0, MAX_OPTION_MS]`, sizes `[0, MAX_OPTION_PIXELS]`,
    /// fractions `[0, 1]`, counts finite with magnitude at most
    /// `MAX_COUNT_MAGNITUDE`, and decimals at most `MAX_COUNT_DECIMALS`.
    /// `beads` is not refused: it is capped at [`MAX_BEADS`].
    ///
    /// # Errors
    ///
    /// [`InkError::InvalidOption`] naming the first field out of domain.
    pub fn validate(&self) -> Result<(), InkError> {
        within(self.duration_ms, 0.0, MAX_OPTION_MS, "duration_ms")?;
        within(self.hold_ms, 0.0, MAX_OPTION_MS, "hold_ms")?;
        within(self.stagger_ms, 0.0, MAX_OPTION_MS, "stagger_ms")?;
        within(self.auto_ms, 0.0, MAX_OPTION_MS, "auto_ms")?;
        within(self.blur, 0.0, MAX_OPTION_PIXELS, "blur")?;
        within(self.displacement, 0.0, MAX_OPTION_PIXELS, "displacement")?;
        within(self.size, 0.0, MAX_OPTION_PIXELS, "size")?;
        within(self.level, 0.0, 1.0, "level")?;
        within(self.spout, 0.0, 1.0, "spout")?;
        within(self.amount, 0.0, MAX_OPTION_AMOUNT, "amount")?;
        within(self.hover, 0.0, MAX_OPTION_AMOUNT, "hover")?;
        within(
            self.count_to,
            -MAX_COUNT_MAGNITUDE,
            MAX_COUNT_MAGNITUDE,
            "count_to",
        )?;
        within(
            self.count_from,
            -MAX_COUNT_MAGNITUDE,
            MAX_COUNT_MAGNITUDE,
            "count_from",
        )?;
        if self
            .count_decimals
            .is_some_and(|decimals| decimals > MAX_COUNT_DECIMALS)
        {
            lgwks_std::trace::debug!(
                decimals = ?self.count_decimals,
                max = MAX_COUNT_DECIMALS,
                "ink option refused: count_decimals is past its bound"
            );
            return Err(InkError::InvalidOption("count_decimals"));
        }
        Ok(())
    }
}

impl Default for Options {
    /// The default options.
    fn default() -> Self {
        Self::new()
    }
}

/// A registered effect host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct HostId(pub u32);

/// A host specification passed to [`InkEngine::add_host`].
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct HostSpec {
    /// The effect to run.
    pub effect: Effect,
    /// Canvas size.
    pub canvas: Canvas,
    /// Whether the host starts on screen.
    pub visible: bool,
}

impl HostSpec {
    /// Build a host specification.
    ///
    /// The struct is `non_exhaustive`, so external crates construct it through
    /// this function rather than a struct literal.
    #[must_use]
    pub const fn new(effect: Effect, canvas: Canvas, visible: bool) -> Self {
        Self {
            effect,
            canvas,
            visible,
        }
    }
}

/// Engine-level counters, mirroring `LWInk.stats()` plus determinism data.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct Stats {
    /// Frames stepped since construction.
    pub frames: u64,
    /// Currently on-screen, unfinished hosts.
    pub live: u32,
    /// Whether the lite tier is active.
    pub lite: bool,
    /// Measured frame-time EMA in milliseconds.
    pub ema_ms: f64,
    /// Accumulated stride skips, a proxy for saved work.
    pub stride_skips: u64,
}

/// One host's render output for a stepped frame.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Render {
    /// The host this frame belongs to.
    pub host: HostId,
    /// The frame to rasterize.
    pub frame: Frame,
}
