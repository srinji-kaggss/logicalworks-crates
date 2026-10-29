//! The engine: per-host state, the seeded random source, and the frame step
//! that stamps each visible host's shapes. Its children hold the per-effect
//! advances and the mark's beam geometry.

mod beams;
mod effects;
#[cfg(test)]
mod tests;

use super::{
    Canvas, Effect, Frame, HostId, HostSpec, InkError, Input, LITE_ENTER_MS, LITE_LEAVE_MS,
    MAX_SHAPES, Options, Quality, Render, Shape, Stats,
};
use beams::{beam_geometry, brand_default, group_thousands};

/// A deterministic `splitmix64` random source.
///
/// Used only where the specification itself is random (spray, bits, idle
/// wander, auto-splash). One seed makes a whole run replayable; the JavaScript
/// engine reads `Math.random` directly.
struct Rng {
    /// The generator state.
    state: u64,
}

impl Rng {
    /// Seed the generator. Zero is remapped so the stream is never degenerate.
    const fn new(seed: u64) -> Self {
        let state = if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        };
        Self { state }
    }

    /// Draw the next 64-bit value.
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut zed = self.state;
        zed = (zed ^ zed.wrapping_shr(30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        zed = (zed ^ zed.wrapping_shr(27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        zed ^ zed.wrapping_shr(31)
    }

    /// Draw a float in `[0, 1)`.
    fn unit(&mut self) -> f64 {
        let [b0, b1, b2, b3, _, _, _, _] = self.next_u64().to_be_bytes();
        f64::from(u32::from_be_bytes([b0, b1, b2, b3])) / 4_294_967_296.0
    }

    /// Draw a float in `[low, high)`.
    fn range(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.unit()
    }
}

/// Clamp to `[0, 1]`.
fn clamp01(value: f64) -> f64 {
    value.clamp(0.0, 1.0)
}

/// The brand's settle easing: `1 - (1 - t)^5`.
fn settle(time: f64) -> f64 {
    1.0 - (1.0 - time).powi(5)
}

/// The brand's cubic in-out easing.
fn in_out(time: f64) -> f64 {
    if time < 0.5 {
        4.0 * time * time * time
    } else {
        let value = -2.0 * time + 2.0;
        1.0 - value.powi(3) / 2.0
    }
}

/// A per-host falling drop.
#[derive(Clone, Copy, Debug)]
struct Drop {
    /// Centre x.
    centre_x: f64,
    /// Centre y.
    centre_y: f64,
    /// Vertical velocity, pixels per millisecond.
    velocity_y: f64,
    /// Radius.
    radius: f64,
    /// Whether the drop has landed in the puddle.
    landed: bool,
}

/// A per-host spray bit.
#[derive(Clone, Copy, Debug)]
struct Bit {
    /// Centre x.
    centre_x: f64,
    /// Centre y.
    centre_y: f64,
    /// Horizontal velocity.
    velocity_x: f64,
    /// Vertical velocity.
    velocity_y: f64,
    /// Radius.
    radius: f64,
}

/// A ripple spot.
#[derive(Clone, Copy, Debug)]
struct Spot {
    /// Centre x.
    centre_x: f64,
    /// Centre y.
    centre_y: f64,
    /// Elapsed time in milliseconds.
    elapsed: f64,
    /// Petal count.
    petals: f64,
    /// Phase offset.
    phase: f64,
}

/// Mutable per-host simulation state.
///
/// Effects share the bag rather than each carrying a distinct type; fields an
/// effect does not use stay at their default.
struct HostState {
    /// Effect-local elapsed time in milliseconds.
    elapsed: f64,
    /// Time left to advance, accumulated between strided frames.
    accumulator: f64,
    /// Wipe/reveal phase: 0 idle, 1 rise, 2 hold, 3 drain, 4 covered.
    phase: u8,
    /// Time in the current wipe phase.
    phase_time: f64,
    /// Wipe level in `[0, 1]`.
    level: f64,
    /// Mark time in milliseconds.
    mark_time: f64,
    /// Hanging drop radius.
    hang: f64,
    /// Puddle height.
    puddle: f64,
    /// Puddle ripple amplitude.
    puddle_amplitude: f64,
    /// Wobble current displacement.
    wobble_current: f64,
    /// Pour displayed level.
    pour_level: f64,
    /// Pour target level.
    pour_target: f64,
    /// Cursor aliveness in `[0, 1]`.
    cursor_alive: f64,
    /// Cursor bead points.
    beads: Vec<(f64, f64)>,
    /// Cursor idle wander phase.
    wander: f64,
    /// Falling drops.
    drops: Vec<Drop>,
    /// Spray bits.
    bits: Vec<Bit>,
    /// Ripple spots.
    spots: Vec<Spot>,
    /// Ripple auto-splash accumulator.
    auto_accumulator: f64,
    /// Wipe column phases.
    columns: Vec<f64>,
    /// Wipe column sizes.
    column_sizes: Vec<f64>,
}

impl HostState {
    /// Empty state, ready for any effect.
    fn new() -> Self {
        Self {
            elapsed: 0.0,
            accumulator: 0.0,
            phase: 0,
            phase_time: 0.0,
            level: 0.0,
            mark_time: 0.0,
            hang: 2.0,
            puddle: 5.0,
            puddle_amplitude: 0.0,
            wobble_current: 0.0,
            pour_level: 0.0,
            pour_target: 0.0,
            cursor_alive: 0.0,
            beads: Vec::new(),
            wander: 0.0,
            // Particle buffers grow on first use, capped by length where they
            // are pushed; an idle or non-particle host holds no heap.
            drops: Vec::new(),
            bits: Vec::new(),
            spots: Vec::new(),
            auto_accumulator: 0.0,
            columns: Vec::new(),
            column_sizes: Vec::new(),
        }
    }
}

/// One registered host.
struct Host {
    /// Stable identity.
    id: HostId,
    /// Effect kind.
    effect: Effect,
    /// Canvas size.
    canvas: Canvas,
    /// Whether the host is on screen.
    visible: bool,
    /// Priority hosts (cursor, ripple, wobble) run every frame.
    priority: bool,
    /// Option overrides.
    options: Options,
    /// Simulation state.
    state: HostState,
    /// Deterministic random source.
    rng: Rng,
    /// Latest input.
    input: Input,
    /// Whether the host has resolved and stopped emitting.
    finished: bool,
}

impl Host {
    /// Add a shape unless the frame budget is already full.
    fn push(frame: &mut Frame, shape: Shape) {
        if frame.shapes.len() < MAX_SHAPES {
            frame.shapes.push(shape);
        }
    }
}

/// The ink lab engine: a bounded set of hosts advanced by an injected clock.
pub struct InkEngine {
    /// Registered hosts.
    hosts: Vec<Host>,
    /// Quality mode.
    quality: Quality,
    /// Whether the lite tier is active.
    lite: bool,
    /// Frame-time exponential moving average, in milliseconds.
    ema_ms: f64,
    /// Engine time at the last quality switch, in milliseconds.
    last_switch_ms: f64,
    /// Total elapsed engine time, in milliseconds.
    clock_ms: f64,
    /// Speed multiplier in thousandths (1000 is 1x).
    speed_milli: u32,
    /// Whether the engine is paused.
    paused: bool,
    /// Whether reduced motion is requested.
    reduce_motion: bool,
    /// Frames stepped.
    frames: u64,
    /// Accumulated stride skips.
    stride_skips: u64,
    /// Engine-level master seed for new hosts.
    seed: u64,
    /// Next host id.
    next_id: u32,
}

impl InkEngine {
    /// Create an engine with a deterministic master seed.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            hosts: Vec::new(),
            quality: Quality::Auto,
            lite: false,
            ema_ms: 16.7,
            last_switch_ms: 0.0,
            clock_ms: 0.0,
            speed_milli: 1000,
            paused: false,
            reduce_motion: false,
            frames: 0,
            stride_skips: 0,
            seed,
            next_id: 0,
        }
    }

    /// Register a host and return its identity.
    pub fn add_host(&mut self, spec: HostSpec) -> HostId {
        let id = HostId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        let host_seed = self
            .seed
            .wrapping_add(u64::from(id.0).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut rng = Rng::new(host_seed);
        let mut state = HostState::new();
        state.columns = Vec::with_capacity(24);
        state.column_sizes = Vec::with_capacity(24);
        for _ in 0..24 {
            state.columns.push(rng.range(0.0, std::f64::consts::TAU));
            state.column_sizes.push(rng.range(16.0, 36.0));
        }
        self.hosts.push(Host {
            id,
            effect: spec.effect,
            canvas: spec.canvas,
            visible: spec.visible,
            priority: matches!(
                spec.effect,
                Effect::Cursor | Effect::Ripple | Effect::Wobble
            ),
            options: Options::new(),
            state,
            rng,
            input: Input::default(),
            finished: false,
        });
        id
    }

    /// Mark a host on screen or off screen.
    pub fn set_visible(&mut self, id: HostId, visible: bool) {
        if let Some(host) = self.hosts.iter_mut().find(|host| host.id == id) {
            host.visible = visible;
            host.finished = false;
        }
    }

    /// Replace the option overrides for a host.
    ///
    /// # Errors
    ///
    /// [`InkError::InvalidOption`] when a field is outside its domain; the
    /// host keeps its previous options.
    pub fn set_options(&mut self, id: HostId, options: Options) -> Result<(), InkError> {
        options.validate()?;
        if let Some(host) = self.hosts.iter_mut().find(|host| host.id == id) {
            host.options = options;
            host.finished = false;
        }
        Ok(())
    }

    /// Deliver input for the next stepped frame.
    ///
    /// # Errors
    ///
    /// [`InkError::InvalidInput`] when a coordinate is not finite; the host
    /// keeps its previous input.
    pub fn set_input(&mut self, id: HostId, input: Input) -> Result<(), InkError> {
        let finite = |point: Option<(f64, f64)>| {
            point.is_none_or(|(point_x, point_y)| point_x.is_finite() && point_y.is_finite())
        };
        if !(finite(input.pointer) && finite(input.click)) {
            lgwks_std::trace::debug!(
                pointer = ?input.pointer,
                click = ?input.click,
                "ink input refused: a coordinate is not finite; the host keeps its previous input"
            );
            return Err(InkError::InvalidInput);
        }
        if let Some(host) = self.hosts.iter_mut().find(|host| host.id == id) {
            host.input = input;
        }
        Ok(())
    }

    /// Pin `Full` or `Lite` rendering, or hand the choice back to the frame
    /// time average with `Auto`; `Lite` takes effect on the next step.
    pub fn set_quality(&mut self, quality: Quality) {
        self.quality = quality;
        self.lite = matches!(quality, Quality::Lite);
    }

    /// Set the speed multiplier in thousandths (1000 is 1x).
    pub fn set_speed_milli(&mut self, speed_milli: u32) {
        self.speed_milli = speed_milli;
    }

    /// Pause or resume the engine.
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// Enable or disable the reduced-motion resolution.
    ///
    /// Changing the preference re-admits every host: turning it on resolves
    /// each effect to its finished frame on the next step, and turning it off
    /// lets resolved effects move again rather than staying frozen.
    pub fn set_reduce_motion(&mut self, reduce_motion: bool) {
        if self.reduce_motion != reduce_motion {
            for host in &mut self.hosts {
                host.finished = false;
            }
        }
        self.reduce_motion = reduce_motion;
    }

    /// Current counters.
    #[must_use]
    pub fn stats(&self) -> Stats {
        let live = self
            .hosts
            .iter()
            .filter(|host| host.visible && !host.finished)
            .fold(0_u32, |live, _| live.saturating_add(1));
        Stats {
            frames: self.frames,
            live,
            lite: self.lite,
            ema_ms: self.ema_ms,
            stride_skips: self.stride_skips,
        }
    }

    /// Advance one frame and return the renders produced this step.
    ///
    /// `dt_ms` is the injected frame delta. Off-screen and finished hosts are
    /// skipped; under the lite tier non-priority hosts are strided so they
    /// advance roughly every second frame. Reduced-motion resolution is never
    /// strided: every visible host settles on the first frame that asks.
    ///
    /// # Errors
    ///
    /// [`InkError::InvalidDelta`] for a negative or non-finite delta; the
    /// engine does not advance.
    pub fn step(&mut self, dt_ms: f64) -> Result<Vec<Render>, InkError> {
        if !(dt_ms.is_finite() && dt_ms >= 0.0) {
            lgwks_std::trace::debug!(
                dt_ms,
                "ink step refused: the delta is negative or not finite"
            );
            return Err(InkError::InvalidDelta);
        }
        self.frames = self.frames.wrapping_add(1);
        self.clock_ms += dt_ms;
        if self.paused {
            return Ok(Vec::new());
        }
        self.ema_ms += (dt_ms - self.ema_ms) * 0.04;
        self.update_quality();
        let speed = f64::from(self.speed_milli) / 1000.0;
        let stride: u64 = if self.lite { 2 } else { 1 };
        let frame_index = self.frames;
        let reduce = self.reduce_motion;
        let mut renders = Vec::with_capacity(self.hosts.len());
        for (index, host) in (0_u64..).zip(self.hosts.iter_mut()) {
            if !host.visible || host.finished {
                host.state.accumulator = 0.0;
                continue;
            }
            if !reduce
                && !host.priority
                && stride > 1
                && !frame_index.wrapping_add(index).is_multiple_of(stride)
            {
                self.stride_skips = self.stride_skips.wrapping_add(1);
                continue;
            }
            host.state.accumulator = (host.state.accumulator + dt_ms * speed).min(200.0);
            let advanced = host.state.accumulator;
            host.state.accumulator = 0.0;
            let frame = if reduce {
                host.resolve()
            } else {
                host.advance(advanced)
            };
            renders.push(Render {
                host: host.id,
                frame,
            });
        }
        Ok(renders)
    }

    /// Update the auto quality tier from the frame-time EMA.
    fn update_quality(&mut self) {
        match self.quality {
            Quality::Lite => self.lite = true,
            Quality::Full => self.lite = false,
            Quality::Auto => {
                let entered = !self.lite
                    && self.ema_ms > LITE_ENTER_MS
                    && self.clock_ms - self.last_switch_ms > 3000.0;
                let recovered = self.lite
                    && self.ema_ms < LITE_LEAVE_MS
                    && self.clock_ms - self.last_switch_ms > 8000.0;
                if entered {
                    self.lite = true;
                    self.last_switch_ms = self.clock_ms;
                } else if recovered {
                    self.lite = false;
                    self.last_switch_ms = self.clock_ms;
                }
            }
        }
    }
}

impl Host {
    /// Advance this host and produce a frame.
    fn advance(&mut self, dt_ms: f64) -> Frame {
        let mut frame = Frame::resolved();
        match self.effect {
            Effect::Mark => self.advance_mark(dt_ms, &mut frame),
            Effect::Text => self.advance_text(dt_ms, &mut frame),
            Effect::Wipe => self.advance_wipe(dt_ms, &mut frame, false),
            Effect::Drops => self.advance_drops(dt_ms, &mut frame),
            Effect::Wobble => self.advance_wobble(dt_ms, &mut frame),
            Effect::Loader => self.advance_loader(dt_ms, &mut frame),
            Effect::Pour => self.advance_pour(dt_ms, &mut frame),
            Effect::Cursor => self.advance_cursor(dt_ms, &mut frame),
            Effect::Ripple => self.advance_ripple(dt_ms, &mut frame),
            Effect::Count => self.advance_count(dt_ms, &mut frame),
            Effect::Reveal => self.advance_wipe(dt_ms, &mut frame, true),
        }
        if self.state.elapsed > 1_000_000.0 {
            self.finished = true;
        }
        frame
    }

    /// Resolve an effect to its crisp end state for reduced motion.
    fn resolve(&mut self) -> Frame {
        self.finished = true;
        match self.effect {
            Effect::Text => Frame {
                shapes: Vec::new(),
                alpha: 1.0,
                blur: 0.0,
                displacement: 0.0,
                label: None,
            },
            Effect::Count => {
                let mut frame = Frame::resolved();
                frame.label = Some(self.count_label(1.0));
                frame
            }
            Effect::Mark => {
                let mut frame = Frame::resolved();
                for beam in beam_geometry() {
                    Host::push(&mut frame, beam.quad());
                }
                frame
            }
            _ => Frame::resolved(),
        }
    }

    /// The count effect's formatted label at a progress value.
    fn count_label(&self, progress: f64) -> String {
        let from = brand_default(self.options.count_from, 0.0);
        let to = brand_default(self.options.count_to, 1200.0);
        let value = from + (to - from) * progress;
        let decimals = brand_default(self.options.count_decimals, 0);
        let number = format!("{value:.decimals$}");
        let prefix = brand_default(self.options.count_prefix, "");
        let suffix = brand_default(self.options.count_suffix, "");
        format!("{prefix}{}{suffix}", group_thousands(&number))
    }
}
