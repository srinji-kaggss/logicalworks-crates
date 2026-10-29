//! The GPU-accelerated runtime: device residency, demand scheduling and the
//! honest accounting that sits between the [`crate::ink_lab`] engine and a
//! real graphics device.
//!
//! This module is the contract half of drishti's presentation residency
//! (requirement R16, journey J13). It does not talk to wgpu or any driver: a
//! real accelerated backend is admitted through `lgwks_deps` and remains an
//! open obligation (G03). What it does prove, deterministically, is the model
//! the backend must satisfy:
//!
//! * **Demand, not visibility, drives work.** An idle runtime submits nothing;
//!   an unchanged ink frame is uploaded once and reused while its digest
//!   holds, so saved bytes are a measured number, not an assumption.
//! * **Budgets are enforced, not advisory.** More shapes, hosts or in-flight
//!   frames than the profile allows return [`RuntimeError::Backpressure`]
//!   instead of silently overcommitting the device.
//! * **Device loss is explicit.** A lost device clears its transfer cache,
//!   drops in-flight work, and is recovered only by an epoch bump that
//!   invalidates every prior submission.
//!
//! Every counter is integer and every operation is deterministic, so a seeded
//! run replays byte for byte.

use crate::ink_lab::{
    HostId, HostSpec, InkEngine, InkError, Input, Options, Quality, Render, Stats,
};

/// A transfer estimate for one shape, in bytes, before device-side packing.
///
/// Shapes are two floats plus a kind tag; the figure is a declared estimate,
/// not a measured driver cost, and exists so upload accounting is a real number
/// a profile can be calibrated against.
pub const BYTES_PER_SHAPE: u64 = 24;

/// A transfer estimate for one frame's filter parameters, in bytes.
pub const BYTES_PER_FILTER: u64 = 64;

/// Bound on the ink-lab hosts a runtime profile admits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Budgets {
    /// Maximum shapes across all hosts in one presented frame.
    pub max_shapes_per_frame: usize,
    /// Maximum uploaded bytes in one presented frame.
    pub max_upload_bytes_per_frame: u64,
    /// Maximum hosts that may be in flight at once.
    pub max_in_flight: u32,
    /// Maximum hosts a runtime admits at all.
    pub max_hosts: u32,
}

impl Budgets {
    /// The conservative reference profile: enough for a dock's ink chrome, not
    /// for a hundred animated tiles.
    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            max_shapes_per_frame: 2048,
            max_upload_bytes_per_frame: 512 * 1024,
            max_in_flight: 16,
            max_hosts: 64,
        }
    }
}

impl Default for Budgets {
    /// The conservative reference profile.
    fn default() -> Self {
        Self::conservative()
    }
}

/// Lifecycle of the presentation device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DeviceState {
    /// Accepting new submissions.
    Ready,
    /// Finishing admitted work; refusing new submissions.
    Draining,
    /// Lost or removed; no submission or transfer is valid until recovery.
    Lost,
}

/// A typed runtime refusal, never a silent degradation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RuntimeError {
    /// The device is lost or draining and cannot accept this frame.
    DeviceUnavailable,
    /// The frame exceeds a profile budget.
    Backpressure,
    /// The runtime already holds its maximum hosts.
    HostLimit,
    /// The engine refused malformed input; nothing was committed.
    Rejected(InkError),
}

/// One host's presentation receipt for a presented frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct FrameReceipt {
    /// The host presented.
    pub host: HostId,
    /// Shapes rasterized this frame.
    pub shapes: usize,
    /// Bytes uploaded for this host, or zero when the digest was reused.
    pub upload_bytes: u64,
    /// Device epoch at submission.
    pub epoch: u64,
    /// The frame digest, the reuse key.
    pub digest: u64,
}

/// The result of one [`Runtime::present`] call.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct RuntimeReport {
    /// The per-host receipts presented this frame; empty when idle.
    receipts: Vec<FrameReceipt>,
    /// Frames whose digest changed: the only content a backend must upload.
    /// A reused receipt has no entry; the backend keeps its resident copy.
    uploads: Vec<Render>,
    /// Whether the frame was idle (no demand, nothing submitted).
    pub idle: bool,
    /// Device epoch at presentation.
    pub epoch: u64,
    /// Total bytes uploaded since construction.
    pub uploaded_bytes: u64,
    /// Hosts whose digest was reused instead of uploaded, cumulative.
    pub reused: u64,
}

impl RuntimeReport {
    /// The per-host receipts for this frame, as a bounded read-only slice.
    #[must_use]
    pub fn receipts(&self) -> &[FrameReceipt] {
        &self.receipts
    }

    /// The frames a backend must upload this present, in receipt order.
    ///
    /// Hosts absent here either reused their resident frame or had no demand.
    /// A backend clears its resident frames whenever [`Self::epoch`] changes.
    #[must_use]
    pub fn uploads(&self) -> &[Render] {
        &self.uploads
    }
}

/// Cumulative runtime counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct RuntimeTotals {
    /// Frames presented.
    pub frames_presented: u64,
    /// Frames presented with no demand.
    pub idle_frames: u64,
    /// Receipts presented.
    pub receipts: u64,
    /// Receipts whose frame was reused.
    pub reused: u64,
    /// Total uploaded bytes.
    pub uploaded_bytes: u64,
    /// Device epoch.
    pub epoch: u64,
    /// Current device state.
    pub state: DeviceState,
}

/// A host's last uploaded frame digest, the reuse cache.
struct CachedFrame {
    /// Host identity.
    host: HostId,
    /// Digest of the last uploaded frame.
    digest: u64,
}

/// A frame already validated against the budgets, before commit.
#[derive(Clone, Copy, Debug)]
struct Pending {
    /// Host identity.
    host: HostId,
    /// Shape count.
    shapes: usize,
    /// Upload bytes if not reused.
    bytes: u64,
    /// Frame digest.
    digest: u64,
}

/// Order statistics over measured frame intervals, in milliseconds.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct Percentiles {
    /// Number of samples.
    pub samples: usize,
    /// Median.
    pub p50: f64,
    /// 95th percentile.
    pub p95: f64,
    /// 99th percentile.
    pub p99: f64,
    /// Largest sample.
    pub max: f64,
}

impl Percentiles {
    /// Compute nearest-rank percentiles. An empty sample set reports zeros
    /// with `samples == 0`, so the caller can see that nothing was measured.
    #[must_use]
    pub fn of(samples: &[f64]) -> Self {
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        let Some(&max) = sorted.last() else {
            return Self {
                samples: 0,
                p50: 0.0,
                p95: 0.0,
                p99: 0.0,
                max: 0.0,
            };
        };
        // Nearest rank: ceil(n * q) - 1, which is below `n` for q <= 1.
        let rank = |permille: usize| -> f64 {
            let index = sorted
                .len()
                .saturating_mul(permille)
                .div_ceil(1000)
                .saturating_sub(1);
            match sorted.get(index) {
                Some(&value) => value,
                None => max,
            }
        };
        Self {
            samples: sorted.len(),
            p50: rank(500),
            p95: rank(950),
            p99: rank(990),
            max,
        }
    }
}

/// A deterministic GPU-residency runtime over an [`InkEngine`].
pub struct Runtime {
    /// The ink engine supplying demand.
    engine: InkEngine,
    /// Admitted limits.
    budgets: Budgets,
    /// Device state.
    state: DeviceState,
    /// Device epoch, bumped on every recovery.
    epoch: u64,
    /// Admitted host count, for the host budget.
    host_count: u32,
    /// Hosts with an uploaded frame.
    cache: Vec<CachedFrame>,
    /// Frames presented.
    frames_presented: u64,
    /// Idle frames.
    idle_frames: u64,
    /// Receipts presented.
    receipts_presented: u64,
    /// Receipts reused.
    reused: u64,
    /// Uploaded bytes.
    uploaded_bytes: u64,
}

impl Runtime {
    /// Create a ready runtime with the given seed and budgets.
    #[must_use]
    pub fn new(seed: u64, budgets: Budgets) -> Self {
        Self {
            engine: InkEngine::new(seed),
            budgets,
            state: DeviceState::Ready,
            epoch: 1,
            host_count: 0,
            cache: Vec::new(),
            frames_presented: 0,
            idle_frames: 0,
            receipts_presented: 0,
            reused: 0,
            uploaded_bytes: 0,
        }
    }

    /// Register an ink host, refusing past the profile's host budget.
    pub fn add_host(&mut self, spec: HostSpec) -> Result<HostId, RuntimeError> {
        if self.host_count >= self.budgets.max_hosts {
            lgwks_std::trace::debug!(
                hosts = self.host_count,
                max_hosts = self.budgets.max_hosts,
                "residency refused a host: the profile's host budget is spent"
            );
            return Err(RuntimeError::HostLimit);
        }
        self.host_count = self.host_count.wrapping_add(1);
        Ok(self.engine.add_host(spec))
    }

    /// Mark a host on or off screen.
    pub fn set_visible(&mut self, id: HostId, visible: bool) {
        self.engine.set_visible(id, visible);
    }

    /// Deliver pointer or click input for the next presented frame.
    ///
    /// # Errors
    ///
    /// [`RuntimeError::Rejected`] for a non-finite coordinate.
    pub fn set_input(&mut self, id: HostId, input: Input) -> Result<(), RuntimeError> {
        self.engine
            .set_input(id, input)
            .map_err(RuntimeError::Rejected)
    }

    /// Replace a host's option overrides.
    ///
    /// # Errors
    ///
    /// [`RuntimeError::Rejected`] for an option outside its domain.
    pub fn set_options(&mut self, id: HostId, options: Options) -> Result<(), RuntimeError> {
        self.engine
            .set_options(id, options)
            .map_err(RuntimeError::Rejected)
    }

    /// Force a quality tier.
    pub fn set_quality(&mut self, quality: Quality) {
        self.engine.set_quality(quality);
    }

    /// Resolve every effect to its crisp end state.
    pub fn set_reduce_motion(&mut self, reduce_motion: bool) {
        self.engine.set_reduce_motion(reduce_motion);
    }

    /// Set the engine speed multiplier in thousandths.
    pub fn set_speed_milli(&mut self, speed_milli: u32) {
        self.engine.set_speed_milli(speed_milli);
    }

    /// Pause or resume demand generation.
    pub fn set_paused(&mut self, paused: bool) {
        self.engine.set_paused(paused);
    }

    /// The current device state.
    #[must_use]
    pub fn state(&self) -> DeviceState {
        self.state
    }

    /// The current device epoch.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Cumulative counters.
    #[must_use]
    pub fn totals(&self) -> RuntimeTotals {
        RuntimeTotals {
            frames_presented: self.frames_presented,
            idle_frames: self.idle_frames,
            receipts: self.receipts_presented,
            reused: self.reused,
            uploaded_bytes: self.uploaded_bytes,
            epoch: self.epoch,
            state: self.state,
        }
    }

    /// The underlying ink engine's counters.
    #[must_use]
    pub fn ink_stats(&self) -> Stats {
        self.engine.stats()
    }

    /// Lose the device: clear transfers, drop in-flight work, refuse frames.
    pub fn lose_device(&mut self) {
        self.state = DeviceState::Lost;
        self.cache.clear();
    }

    /// Begin draining: finish admitted work, refuse new submissions.
    pub fn drain(&mut self) {
        if self.state == DeviceState::Ready {
            self.state = DeviceState::Draining;
        }
    }

    /// Recover a lost or drained device with a fresh epoch.
    ///
    /// The epoch bump is what invalidates every prior submission; nothing from
    /// the old device may be reused after recovery.
    pub fn recover(&mut self) -> u64 {
        self.epoch = self.epoch.wrapping_add(1);
        self.cache.clear();
        self.state = DeviceState::Ready;
        self.epoch
    }

    /// Advance one frame and present it under the profile's budgets.
    ///
    /// Returns [`RuntimeError::DeviceUnavailable`] while lost or draining,
    /// [`RuntimeError::Rejected`] for a malformed delta, and
    /// [`RuntimeError::Backpressure`] when the frame would exceed a budget.
    /// A refused frame commits nothing: no cache, transfer or counter moves.
    pub fn present(&mut self, dt_ms: f64) -> Result<RuntimeReport, RuntimeError> {
        if self.state != DeviceState::Ready {
            lgwks_std::trace::debug!(
                state = ?self.state,
                epoch = self.epoch,
                "residency refused a present: the device is not ready"
            );
            return Err(RuntimeError::DeviceUnavailable);
        }
        let renders: Vec<Render> = self.engine.step(dt_ms).map_err(RuntimeError::Rejected)?;
        // `renders` is consumed in receipt order below; `pending` mirrors it.
        if renders.is_empty() {
            self.idle_frames = self.idle_frames.wrapping_add(1);
            return Ok(RuntimeReport {
                receipts: Vec::new(),
                uploads: Vec::new(),
                idle: true,
                epoch: self.epoch,
                uploaded_bytes: self.uploaded_bytes,
                reused: self.reused,
            });
        }
        // A count past `u32::MAX` is past any budget, so it is refused too.
        let within_flight = matches!(
            u32::try_from(renders.len()),
            Ok(in_flight) if in_flight <= self.budgets.max_in_flight
        );
        if !within_flight {
            lgwks_std::trace::debug!(
                renders = renders.len(),
                max_in_flight = self.budgets.max_in_flight,
                "residency refused a present: more renders than may be in flight"
            );
            return Err(RuntimeError::Backpressure);
        }
        let mut pending = Vec::with_capacity(renders.len());
        let mut total_shapes: usize = 0;
        let mut total_bytes: u64 = 0;
        for render in &renders {
            let shapes = render.frame.shapes().len();
            let shape_bytes = match u64::try_from(shapes) {
                Ok(count) => count.saturating_mul(BYTES_PER_SHAPE),
                Err(error) => {
                    lgwks_std::trace::debug!(
                        shapes,
                        %error,
                        "residency refused a present: a shape count past any budget"
                    );
                    return Err(RuntimeError::Backpressure);
                }
            };
            let filter_bytes = if render.frame.blur > 0.0 || render.frame.displacement > 0.0 {
                BYTES_PER_FILTER
            } else {
                0
            };
            let bytes = shape_bytes.saturating_add(filter_bytes);
            total_shapes = total_shapes.saturating_add(shapes);
            total_bytes = total_bytes.saturating_add(bytes);
            pending.push(Pending {
                host: render.host,
                shapes,
                bytes,
                digest: render.frame.digest(),
            });
        }
        if total_shapes > self.budgets.max_shapes_per_frame
            || total_bytes > self.budgets.max_upload_bytes_per_frame
        {
            lgwks_std::trace::debug!(
                total_shapes,
                total_bytes,
                max_shapes = self.budgets.max_shapes_per_frame,
                max_bytes = self.budgets.max_upload_bytes_per_frame,
                "residency refused a present: the frame is past a budget"
            );
            return Err(RuntimeError::Backpressure);
        }
        let mut receipts = Vec::with_capacity(pending.len());
        let mut uploads = Vec::with_capacity(pending.len());
        let mut frame_bytes: u64 = 0;
        for (item, render) in pending.into_iter().zip(renders) {
            let (receipt, reused) = self.admit(item);
            frame_bytes = frame_bytes.saturating_add(receipt.upload_bytes);
            // A changed digest replaces the resident frame even when the new
            // frame is empty (a resolved effect): zero bytes is still news.
            if !reused {
                uploads.push(render);
            }
            receipts.push(receipt);
        }
        self.uploaded_bytes = self.uploaded_bytes.saturating_add(frame_bytes);
        self.frames_presented = self.frames_presented.wrapping_add(1);
        self.receipts_presented = receipts
            .iter()
            .fold(self.receipts_presented, |count, _| count.saturating_add(1));
        Ok(RuntimeReport {
            receipts,
            uploads,
            idle: false,
            epoch: self.epoch,
            uploaded_bytes: self.uploaded_bytes,
            reused: self.reused,
        })
    }

    /// Produce a receipt for one host frame, reusing an unchanged upload, and
    /// say whether it was reused.
    fn admit(&mut self, item: Pending) -> (FrameReceipt, bool) {
        let reused = match self
            .cache
            .iter_mut()
            .find(|cached| cached.host == item.host)
        {
            Some(cached) => {
                let unchanged = cached.digest == item.digest;
                cached.digest = item.digest;
                unchanged
            }
            None => false,
        };
        if reused {
            self.reused = self.reused.wrapping_add(1);
            let receipt = FrameReceipt {
                host: item.host,
                shapes: item.shapes,
                upload_bytes: 0,
                epoch: self.epoch,
                digest: item.digest,
            };
            (receipt, true)
        } else {
            // One entry per admitted host; `add_host` bounds hosts at
            // `max_hosts`, so the cache cannot outgrow the profile.
            if !self.cache.iter().any(|cached| cached.host == item.host) {
                self.cache.push(CachedFrame {
                    host: item.host,
                    digest: item.digest,
                });
            }
            let receipt = FrameReceipt {
                host: item.host,
                shapes: item.shapes,
                upload_bytes: item.bytes,
                epoch: self.epoch,
                digest: item.digest,
            };
            (receipt, false)
        }
    }
}

impl std::fmt::Debug for Runtime {
    /// Runtime debug summary.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Runtime")
            .field("state", &self.state)
            .field("epoch", &self.epoch)
            .field("budgets", &self.budgets)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{Budgets, DeviceState, HostId, Runtime, RuntimeError, RuntimeReport};
    use crate::ink_lab::{Canvas, Effect, HostSpec, Input};

    /// A runtime with one loader host.
    fn runtime_with_loader(seed: u64) -> (Runtime, HostId) {
        let mut runtime = Runtime::new(seed, Budgets::conservative());
        let id = admitted(runtime.add_host(HostSpec {
            effect: Effect::Loader,
            canvas: Canvas::new(120.0, 120.0),
            visible: true,
        }));
        (runtime, id)
    }

    /// The admitted host id; a refusal fails the test rather than inventing one.
    fn admitted(result: Result<HostId, RuntimeError>) -> HostId {
        assert!(result.is_ok(), "host refused: {result:?}");
        match result {
            Ok(host) => host,
            Err(_) => HostId(u32::MAX),
        }
    }

    /// Unwrap a report without `unwrap`/`expect`/`panic`.
    fn presented(result: Result<RuntimeReport, RuntimeError>) -> RuntimeReport {
        let succeeded = result.is_ok();
        match result {
            Ok(report) => report,
            Err(error) => {
                assert!(succeeded, "present refused: {error:?}");
                RuntimeReport {
                    receipts: Vec::new(),
                    uploads: Vec::new(),
                    idle: true,
                    epoch: 0,
                    uploaded_bytes: 0,
                    reused: 0,
                }
            }
        }
    }

    #[test]
    fn idle_runtime_submits_nothing() {
        let mut runtime = Runtime::new(1, Budgets::conservative());
        let report = presented(runtime.present(16.0));
        assert!(report.idle);
        assert!(report.receipts().is_empty());
        assert_eq!(runtime.totals().idle_frames, 1);
    }

    #[test]
    fn idle_frames_do_not_upload() {
        let (mut runtime, _) = runtime_with_loader(4);
        let _ = presented(runtime.present(0.0));
        let before = runtime.totals().uploaded_bytes;
        runtime.set_paused(true);
        let report = presented(runtime.present(16.0));
        assert!(report.idle, "paused engine should be idle");
        assert_eq!(
            runtime.totals().uploaded_bytes,
            before,
            "idle advanced uploads"
        );
    }

    #[test]
    fn budget_pressure_is_refused_not_silently_dropped() {
        let budgets = Budgets {
            max_in_flight: 1,
            ..Budgets::conservative()
        };
        let mut runtime = Runtime::new(2, budgets);
        for _ in 0..3 {
            assert!(
                runtime
                    .add_host(HostSpec {
                        effect: Effect::Loader,
                        canvas: Canvas::new(80.0, 80.0),
                        visible: true,
                    })
                    .is_ok(),
                "host budget should admit three hosts"
            );
        }
        assert!(
            matches!(runtime.present(16.0), Err(RuntimeError::Backpressure)),
            "over-budget frame was not refused"
        );
        assert_eq!(
            runtime.totals().uploaded_bytes,
            0,
            "refused frame committed bytes"
        );
    }

    #[test]
    fn device_loss_refuses_frames_until_epoch_bump() {
        let (mut runtime, _) = runtime_with_loader(6);
        let _ = presented(runtime.present(16.0));
        runtime.lose_device();
        assert_eq!(runtime.state(), DeviceState::Lost);
        assert!(
            matches!(runtime.present(16.0), Err(RuntimeError::DeviceUnavailable)),
            "lost device accepted a frame"
        );
        let before = runtime.epoch();
        let after = runtime.recover();
        assert_eq!(after, before.wrapping_add(1), "recovery must bump epoch");
        assert!(runtime.present(16.0).is_ok());
    }

    #[test]
    fn uploads_carry_exactly_the_changed_frames() {
        let (mut runtime, id) = runtime_with_loader(8);
        runtime.set_reduce_motion(true);
        let first = presented(runtime.present(16.0));
        assert_eq!(first.uploads().len(), 1, "first frame must upload");
        assert_eq!(first.uploads()[0].host, id, "upload belongs to the host");
        // The resolved loader is an empty frame: zero bytes, yet still an
        // upload, because it must replace whatever the backend had resident.
        assert!(
            first.uploads()[0].frame.shapes().is_empty(),
            "resolved loader is empty"
        );
        // The resolved loader is finished: no demand, so nothing uploads.
        let second = presented(runtime.present(16.0));
        assert!(second.idle, "a resolved host has no demand");
        assert!(second.uploads().is_empty(), "idle frame uploaded");
    }

    #[test]
    fn a_reused_frame_is_receipted_but_not_uploaded() {
        let mut runtime = Runtime::new(5, Budgets::conservative());
        let id = admitted(runtime.add_host(HostSpec {
            effect: Effect::Mark,
            canvas: Canvas::new(100.0, 100.0),
            visible: true,
        }));
        // The mark is crisp from 1850 ms until its hold ends at 3700 ms, so
        // consecutive frames in that window are identical.
        for _ in 0..125 {
            let _ = presented(runtime.present(16.0));
        }
        let held = presented(runtime.present(16.0));
        assert_eq!(held.receipts().len(), 1, "the mark has demand");
        assert_eq!(held.receipts()[0].host, id, "receipt belongs to the mark");
        assert_eq!(held.receipts()[0].upload_bytes, 0, "held mark re-uploaded");
        assert!(
            held.uploads().is_empty(),
            "held mark delivered as an upload"
        );
    }

    #[test]
    fn host_limit_is_enforced() {
        let budgets = Budgets {
            max_hosts: 1,
            ..Budgets::conservative()
        };
        let mut runtime = Runtime::new(3, budgets);
        let spec = HostSpec {
            effect: Effect::Loader,
            canvas: Canvas::new(50.0, 50.0),
            visible: true,
        };
        assert!(runtime.add_host(spec).is_ok());
        assert_eq!(runtime.add_host(spec), Err(RuntimeError::HostLimit));
    }

    #[test]
    fn seeded_runtimes_replay_identical_frames() {
        let run = |seed: u64| {
            let mut runtime = Runtime::new(seed, Budgets::conservative());
            let id = admitted(runtime.add_host(HostSpec {
                effect: Effect::Ripple,
                canvas: Canvas::new(120.0, 120.0),
                visible: true,
            }));
            let mut trace = 0_u64;
            for _ in 0..20 {
                let accepted = runtime.set_input(
                    id,
                    Input {
                        pointer: None,
                        click: Some((30.0, 30.0)),
                    },
                );
                assert!(accepted.is_ok(), "finite click refused");
                let report = presented(runtime.present(16.0));
                for receipt in report.receipts() {
                    trace = trace.wrapping_mul(31).wrapping_add(receipt.digest);
                }
            }
            trace
        };
        assert_eq!(run(11), run(11), "replay diverged");
        assert_ne!(run(11), run(12), "seed did not change the trace");
    }
}
