//! The deterministic simulation harness.
//!
//! One seed controls everything a run sees: the frame clock (including
//! zero, huge, negative and non-finite deltas), pointer and click input,
//! visibility, quality, pause, speed, hostile option values, device loss,
//! drain and recovery, and host admission past the budget. The harness
//! drives the real public `Runtime` API, checks every invariant after every
//! operation, and folds every observable outcome into a trace hash, so the
//! same seed must replay to the same hash.

use drishti::ink_lab::{
    Canvas, Effect, HostId, HostSpec, InkError, Input, MAX_SHAPES, Options, Quality, Shape,
};
use drishti::runtime::{Budgets, DeviceState, Runtime, RuntimeError, RuntimeReport};

/// A `splitmix64` source: the only randomness a simulation may use.
pub struct Rng {
    /// Generator state.
    state: u64,
}

impl Rng {
    /// Seed the source.
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// The next 64-bit value.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut mixed = self.state;
        mixed = (mixed ^ mixed.wrapping_shr(30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        mixed = (mixed ^ mixed.wrapping_shr(27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        mixed ^ mixed.wrapping_shr(31)
    }

    /// A value in `[0, bound)`; `bound` zero yields zero.
    pub fn below(&mut self, bound: u64) -> u64 {
        let draw = self.next_u64();
        match std::num::NonZeroU64::new(bound) {
            Some(bound) => draw % bound,
            None => 0,
        }
    }

    /// A `u32` in `[0, bound)`; `bound` zero yields zero.
    pub fn below_u32(&mut self, bound: u32) -> u32 {
        let [b0, b1, b2, b3, _, _, _, _] = self.next_u64().to_be_bytes();
        let draw = u32::from_be_bytes([b0, b1, b2, b3]);
        match std::num::NonZeroU32::new(bound) {
            Some(bound) => draw % bound,
            None => 0,
        }
    }

    /// A `u16` in `[0, bound)`; `bound` zero yields zero.
    pub fn below_u16(&mut self, bound: u16) -> u16 {
        let [b0, b1, _, _, _, _, _, _] = self.next_u64().to_be_bytes();
        let draw = u16::from_be_bytes([b0, b1]);
        match std::num::NonZeroU16::new(bound) {
            Some(bound) => draw % bound,
            None => 0,
        }
    }

    /// A float in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        let [b0, b1, b2, b3, _, _, _, _] = self.next_u64().to_be_bytes();
        f64::from(u32::from_be_bytes([b0, b1, b2, b3])) / 4_294_967_296.0
    }

    /// A float in `[low, high)`.
    pub fn range(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.unit()
    }

    /// Pick one of `items`.
    pub fn pick<T: Copy>(&mut self, items: &[T]) -> Option<T> {
        let length = u64::try_from(items.len()).ok()?;
        let index = usize::try_from(self.below(length)).ok()?;
        items.get(index).copied()
    }
}

/// Fold a value into a trace hash (FNV-1a over 64-bit words).
pub fn mix(hash: u64, value: u64) -> u64 {
    (hash ^ value).wrapping_mul(0x0000_0100_0000_01b3)
}

/// The initial trace hash.
pub const TRACE_SEED: u64 = 0xcbf2_9ce4_8422_2325;

/// Which faults a scenario injects.
#[derive(Clone, Copy, Debug)]
pub struct Faults {
    /// Non-finite, negative and huge frame deltas.
    pub malformed_deltas: bool,
    /// NaN, infinite, negative and huge option values.
    pub hostile_options: bool,
    /// Device loss, drain and recovery.
    pub device_faults: bool,
    /// Tight budgets that force backpressure.
    pub tight_budgets: bool,
}

impl Faults {
    /// Every fault on.
    pub const ALL: Self = Self {
        malformed_deltas: true,
        hostile_options: true,
        device_faults: true,
        tight_budgets: true,
    };
}

/// A simulated tenant: one runtime and the state the harness tracks for it.
pub struct Tenant {
    /// The runtime under test.
    runtime: Runtime,
    /// Its budgets, for invariant checks.
    budgets: Budgets,
    /// Admitted hosts.
    hosts: Vec<HostId>,
    /// Effects this tenant may host.
    effects: Vec<Effect>,
    /// Whether reduced motion is currently on.
    reduce_motion: bool,
    /// The scenario's faults.
    faults: Faults,
    /// The tenant's own randomness.
    rng: Rng,
    /// The trace hash so far.
    trace: u64,
    /// Operations run.
    operations: u64,
    /// Presents refused.
    refusals: u64,
    /// The most recent present refusal, for failure messages.
    last_refusal: Option<RuntimeError>,
    /// Option sets refused as out of domain.
    rejected_options: u64,
}

impl Tenant {
    /// A tenant seeded by `seed`, hosting only `effects`.
    pub fn new(seed: u64, effects: &[Effect], faults: Faults) -> Self {
        let mut rng = Rng::new(seed);
        let mut budgets = Budgets::conservative();
        if faults.tight_budgets && rng.below(2) == 0 {
            budgets.max_in_flight = rng.below_u32(4).saturating_add(1);
            budgets.max_shapes_per_frame = usize::from(rng.below_u16(600)).saturating_add(64);
            budgets.max_upload_bytes_per_frame = rng.below(16 * 1024).saturating_add(256);
            budgets.max_hosts = rng.below_u32(6).saturating_add(1);
        }
        let mut tenant = Self {
            runtime: Runtime::new(seed, budgets),
            budgets,
            hosts: Vec::new(),
            effects: effects.to_vec(),
            reduce_motion: false,
            faults,
            rng,
            trace: mix(TRACE_SEED, seed),
            operations: 0,
            refusals: 0,
            last_refusal: None,
            rejected_options: 0,
        };
        let initial = tenant.rng.below(3).saturating_add(1);
        for _ in 0..initial {
            tenant.add_host();
        }
        tenant
    }

    /// The trace hash so far.
    pub const fn trace(&self) -> u64 {
        self.trace
    }

    /// Presents refused so far.
    pub const fn refusals(&self) -> u64 {
        self.refusals
    }

    /// The most recent present refusal, if any.
    pub const fn last_refusal(&self) -> Option<RuntimeError> {
        self.last_refusal
    }

    /// Option sets refused as out of domain so far.
    pub const fn rejected_options(&self) -> u64 {
        self.rejected_options
    }

    /// The runtime, for scenario-specific checks.
    pub const fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// Admit one host, checking the host budget.
    fn add_host(&mut self) {
        let Some(effect) = self.rng.pick(&self.effects) else {
            return;
        };
        let canvas = Canvas::new(self.rng.range(4.0, 400.0), self.rng.range(4.0, 300.0));
        let before = self.hosts.len();
        match self.runtime.add_host(HostSpec::new(effect, canvas, true)) {
            Ok(host) => {
                assert!(
                    matches!(u32::try_from(before), Ok(count) if count < self.budgets.max_hosts),
                    "admitted a host past max_hosts"
                );
                self.hosts.push(host);
                self.trace = mix(self.trace, u64::from(host.0));
            }
            Err(error) => {
                assert_eq!(error, RuntimeError::HostLimit, "unexpected admission error");
                assert!(
                    matches!(u32::try_from(before), Ok(count) if count >= self.budgets.max_hosts),
                    "refused a host below max_hosts"
                );
                self.trace = mix(self.trace, 0xADD);
            }
        }
    }

    /// Run `count` seeded operations.
    pub fn run(&mut self, count: u64) {
        for _ in 0..count {
            self.operate();
        }
    }

    /// Run one seeded operation and check every invariant.
    pub fn operate(&mut self) {
        self.operations = self.operations.wrapping_add(1);
        let roll = self.rng.below(1000);
        let host = self.rng.pick(&self.hosts);
        match (roll, host) {
            (0..600, _) | (_, None) => {
                let dt = self.delta();
                self.present(dt);
            }
            (600..700, Some(id)) => {
                let mut input = Input::default();
                if self.rng.below(5) != 0 {
                    input.pointer =
                        Some((self.rng.range(-50.0, 450.0), self.rng.range(-50.0, 350.0)));
                }
                if self.faults.malformed_deltas && self.rng.below(10) == 0 {
                    input.pointer = Some((f64::NAN, self.rng.range(0.0, 300.0)));
                }
                self.deliver(id, input);
            }
            (700..750, Some(id)) => {
                let mut input = Input::default();
                input.click = Some((self.rng.range(0.0, 400.0), self.rng.range(0.0, 300.0)));
                if self.faults.malformed_deltas && self.rng.below(10) == 0 {
                    input.click = Some((f64::INFINITY, 0.0));
                }
                self.deliver(id, input);
            }
            (750..790, Some(id)) => self.runtime.set_visible(id, self.rng.below(3) != 0),
            (790..820, _) => {
                self.reduce_motion = !self.reduce_motion;
                self.runtime.set_reduce_motion(self.reduce_motion);
            }
            (820..840, _) => {
                if let Some(quality) = self
                    .rng
                    .pick(&[Quality::Auto, Quality::Lite, Quality::Full])
                {
                    self.runtime.set_quality(quality);
                }
            }
            (840..860, _) => self.runtime.set_paused(self.rng.below(4) == 0),
            (860..880, _) => {
                let speed = self.rng.below_u32(4000);
                self.runtime.set_speed_milli(speed);
            }
            (880..920, Some(id)) => {
                let (options, valid) = self.options();
                let result = self.runtime.set_options(id, options);
                assert_eq!(
                    result.is_ok(),
                    valid,
                    "option domain oracle disagrees: {result:?} for {options:?}"
                );
                assert!(
                    result.is_ok()
                        || matches!(
                            result,
                            Err(RuntimeError::Rejected(InkError::InvalidOption(_)))
                        ),
                    "options refused with the wrong error: {result:?}"
                );
                if !valid {
                    self.rejected_options = self.rejected_options.saturating_add(1);
                }
                self.trace = mix(self.trace, u64::from(valid));
            }
            (920..940, _) if self.faults.device_faults => self.lose(),
            (940..955, _) if self.faults.device_faults => self.runtime.drain(),
            (955..980, _) if self.faults.device_faults => self.recover(),
            (980..1000, _) => self.add_host(),
            _ => {
                let dt = self.delta();
                self.present(dt);
            }
        }
        self.trace = mix(self.trace, roll);
    }

    /// A frame delta: usually a plausible frame, sometimes malformed.
    fn delta(&mut self) -> f64 {
        if self.faults.malformed_deltas {
            match self.rng.below(40) {
                0 => return f64::NAN,
                1 => return f64::INFINITY,
                2 => return -self.rng.range(0.0, 1000.0),
                3 => return self.rng.range(1000.0, 1.0e9),
                4 => return 0.0,
                _ => {}
            }
        }
        self.rng.range(4.0, 40.0)
    }

    /// Option overrides, hostile when the scenario asks for it, with the
    /// oracle's verdict on whether every field is inside its documented
    /// domain (docs/inklab.md), computed independently of the engine.
    fn options(&mut self) -> (Options, bool) {
        let mut options = Options::new();
        let hostile = self.faults.hostile_options && self.rng.below(2) == 0;
        let mut valid = true;
        let mut value = |rng: &mut Rng, sane: (f64, f64), domain: (f64, f64)| -> Option<f64> {
            let number = if hostile {
                match rng.below(5) {
                    0 => f64::NAN,
                    1 => f64::INFINITY,
                    2 => -rng.range(0.0, 1.0e6),
                    3 => rng.range(1.0e13, 1.0e14),
                    _ => rng.range(sane.0, sane.1),
                }
            } else {
                rng.range(sane.0, sane.1)
            };
            valid &= number.is_finite() && number >= domain.0 && number <= domain.1;
            Some(number)
        };
        let time = (0.0, 3.6e6);
        let unit = (0.0, 1.0);
        let count = (-1.0e15, 1.0e15);
        options.duration_ms = value(&mut self.rng, (200.0, 3000.0), time);
        options.hold_ms = value(&mut self.rng, (0.0, 2000.0), time);
        options.stagger_ms = value(&mut self.rng, (0.0, 120.0), time);
        options.level = value(&mut self.rng, (0.0, 1.0), unit);
        options.size = value(&mut self.rng, (2.0, 120.0), (0.0, 1.0e4));
        options.spout = value(&mut self.rng, (0.0, 1.0), unit);
        options.auto_ms = value(&mut self.rng, (0.0, 3000.0), time);
        options.amount = value(&mut self.rng, (0.0, 10.0), (0.0, 1.0e3));
        options.hover = value(&mut self.rng, (0.0, 20.0), (0.0, 1.0e3));
        options.count_to = value(&mut self.rng, (0.0, 1.0e6), count);
        options.count_from = value(&mut self.rng, (0.0, 1.0e3), count);
        let decimals = self.rng.below_u16(if hostile { 400 } else { 4 });
        valid &= decimals <= 12;
        options.count_decimals = Some(usize::from(decimals));
        options.beads = Some(if hostile { usize::MAX } else { 8 });
        options.loop_forever = Some(self.rng.below(2) == 0);
        (options, valid)
    }

    /// Deliver input; the oracle says it is refused exactly when a
    /// coordinate is not finite.
    fn deliver(&mut self, id: HostId, input: Input) {
        let finite = [input.pointer, input.click]
            .iter()
            .flatten()
            .all(|&(point_x, point_y)| point_x.is_finite() && point_y.is_finite());
        let result = self.runtime.set_input(id, input);
        let expected = if finite {
            Ok(())
        } else {
            Err(RuntimeError::Rejected(InkError::InvalidInput))
        };
        assert_eq!(result, expected, "input oracle disagrees for {input:?}");
        self.trace = mix(self.trace, u64::from(finite));
    }

    /// Lose the device and check that frames are refused until recovery.
    fn lose(&mut self) {
        self.runtime.lose_device();
        assert_eq!(
            self.runtime.state(),
            DeviceState::Lost,
            "lose_device did not lose"
        );
        self.trace = mix(self.trace, 0x1055);
    }

    /// Recover and check the epoch moved by exactly one.
    fn recover(&mut self) {
        let before = self.runtime.epoch();
        let after = self.runtime.recover();
        assert_eq!(
            after,
            before.wrapping_add(1),
            "recovery must bump the epoch once"
        );
        assert_eq!(
            self.runtime.state(),
            DeviceState::Ready,
            "recovery must ready the device"
        );
        self.trace = mix(self.trace, after);
    }

    /// Present one frame and check every report invariant.
    fn present(&mut self, dt: f64) {
        let before = self.runtime.totals();
        let state = self.runtime.state();
        let valid = dt.is_finite() && dt >= 0.0;
        match self.runtime.present(dt) {
            Ok(report) => {
                assert_eq!(state, DeviceState::Ready, "a non-ready device presented");
                assert!(valid, "malformed delta {dt} was presented");
                self.check(&report, before);
            }
            Err(error) => {
                self.refusals = self.refusals.saturating_add(1);
                self.last_refusal = Some(error);
                assert_eq!(
                    self.runtime.totals(),
                    before,
                    "a refused frame committed counters"
                );
                let consistent = match error {
                    RuntimeError::DeviceUnavailable => state != DeviceState::Ready,
                    RuntimeError::Backpressure => state == DeviceState::Ready && valid,
                    RuntimeError::Rejected(InkError::InvalidDelta) => {
                        state == DeviceState::Ready && !valid
                    }
                    RuntimeError::HostLimit | RuntimeError::Rejected(_) | _ => false,
                };
                assert!(
                    consistent,
                    "present refused with {error:?} on a {state:?} device"
                );
                self.trace = mix(self.trace, error_code(error));
            }
        }
    }

    /// The invariants of one accepted present.
    fn check(&mut self, report: &RuntimeReport, before: drishti::runtime::RuntimeTotals) {
        let after = self.runtime.totals();
        let receipts = report.receipts();
        let uploads = report.uploads();
        assert_eq!(report.epoch, self.runtime.epoch(), "report epoch is stale");
        if report.idle {
            assert!(
                receipts.is_empty() && uploads.is_empty(),
                "idle frame carried work"
            );
            assert_eq!(
                after.idle_frames,
                before.idle_frames.wrapping_add(1),
                "idle not counted"
            );
            self.trace = mix(self.trace, 0x1D1E);
            return;
        }
        assert!(
            matches!(u32::try_from(receipts.len()), Ok(count) if count <= self.budgets.max_in_flight),
            "more receipts than max_in_flight"
        );
        let shapes = receipts.iter().fold(0_usize, |total, receipt| {
            total.saturating_add(receipt.shapes)
        });
        assert!(
            shapes <= self.budgets.max_shapes_per_frame,
            "frame exceeded max_shapes"
        );
        let bytes = receipts.iter().fold(0_u64, |total, receipt| {
            total.saturating_add(receipt.upload_bytes)
        });
        assert!(
            bytes <= self.budgets.max_upload_bytes_per_frame,
            "frame exceeded upload budget"
        );
        assert_eq!(
            after.uploaded_bytes,
            before.uploaded_bytes.saturating_add(bytes),
            "byte counter drifted"
        );
        let reused = receipts.len().saturating_sub(uploads.len());
        assert_eq!(
            u64::try_from(reused).ok(),
            Some(after.reused.saturating_sub(before.reused)),
            "reuse counter disagrees with receipts minus uploads"
        );
        for upload in uploads {
            let frame = &upload.frame;
            let receipt = receipts.iter().find(|receipt| receipt.host == upload.host);
            assert!(
                receipt.is_some_and(|receipt| receipt.digest == frame.digest()),
                "upload without a matching receipt"
            );
            assert!(
                frame.shapes().len() <= MAX_SHAPES,
                "host frame exceeded MAX_SHAPES"
            );
            assert!(
                frame.alpha.is_finite() && (0.0..=1.0).contains(&frame.alpha),
                "alpha out of range"
            );
            assert!(frame.blur.is_finite() && frame.blur >= 0.0, "blur invalid");
            assert!(frame.displacement.is_finite(), "displacement not finite");
            for shape in frame.shapes() {
                assert!(finite(*shape), "non-finite geometry {shape:?}");
            }
            if self.reduce_motion {
                assert!(
                    frame.blur.abs() < 1e-12
                        && frame.displacement.abs() < 1e-12
                        && (frame.alpha - 1.0).abs() < 1e-12,
                    "reduced motion shipped a mid-liquid frame"
                );
            }
        }
        for receipt in receipts {
            assert_eq!(receipt.epoch, report.epoch, "receipt from another epoch");
            if receipt.upload_bytes > 0 {
                assert!(
                    uploads.iter().any(|upload| upload.host == receipt.host),
                    "bytes charged without an upload"
                );
            }
            self.trace = mix(
                mix(mix(self.trace, u64::from(receipt.host.0)), receipt.digest),
                receipt.upload_bytes,
            );
        }
    }
}

/// Whether every coordinate of a shape is finite.
fn finite(shape: Shape) -> bool {
    match shape {
        Shape::Circle {
            centre_x,
            centre_y,
            radius,
        } => [centre_x, centre_y, radius]
            .iter()
            .all(|value| value.is_finite()),
        Shape::Ellipse {
            centre_x,
            centre_y,
            radius_x,
            radius_y,
        } => [centre_x, centre_y, radius_x, radius_y]
            .iter()
            .all(|value| value.is_finite()),
        Shape::Quad { points } => points.iter().flatten().all(|value| value.is_finite()),
        Shape::Rect {
            left,
            top,
            right,
            bottom,
        } => [left, top, right, bottom]
            .iter()
            .all(|value| value.is_finite()),
        _ => false,
    }
}

/// A stable code for a refusal, for the trace hash.
fn error_code(error: RuntimeError) -> u64 {
    match error {
        RuntimeError::DeviceUnavailable => 1,
        RuntimeError::Backpressure => 2,
        RuntimeError::HostLimit => 3,
        RuntimeError::Rejected(_) => 4,
        _ => 5,
    }
}

/// Run one seed to completion and return its trace hash.
pub fn trace_of(seed: u64, effects: &[Effect], faults: Faults, operations: u64) -> u64 {
    let mut tenant = Tenant::new(seed, effects, faults);
    tenant.run(operations);
    tenant.trace()
}
