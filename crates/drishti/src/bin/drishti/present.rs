//! `drishti present` and `drishti limits`: the residency runtime driven through
//! a whole session, and the bounds the engine and the runtime declare.
//!
//! A session runs every effect as a host under the conservative budgets, then
//! walks the device through the lifecycle a backend must survive: a third of
//! the way in it drains and is lost, so presents are refused, then recovers on a
//! fresh epoch; two thirds in, half the hosts leave the screen; at the end
//! reduced motion resolves everything and the runtime goes idle. Every present
//! is timed, and the receipts are folded into a digest one seed reproduces.

use std::time::Instant;

use drishti::ink_lab::{
    Canvas, Effect, HostSpec, Input, LITE_ENTER_MS, LITE_LEAVE_MS, MAX_BEADS, MAX_CANVAS,
    MAX_COUNT_DECIMALS, MAX_COUNT_MAGNITUDE, MAX_OPTION_AMOUNT, MAX_OPTION_MS, MAX_OPTION_PIXELS,
    MAX_SHAPES, Options, Quality,
};
use drishti::runtime::{
    BYTES_PER_FILTER, BYTES_PER_SHAPE, Budgets, DeviceState, FrameReceipt, Percentiles, Runtime,
    RuntimeError, RuntimeReport, RuntimeTotals,
};
use lgwks_std::json::{Value, json};

use crate::ink::{FRAME_MS, fold};

/// A device state's name.
const fn state_name(state: DeviceState) -> &'static str {
    match state {
        DeviceState::Ready => "ready",
        DeviceState::Draining => "draining",
        _ => "lost",
    }
}

/// The cumulative counters, as the document reports them.
pub(crate) fn totals(totals: &RuntimeTotals) -> Value {
    json!({
        "frames_presented": totals.frames_presented,
        "idle_frames": totals.idle_frames,
        "receipts": totals.receipts,
        "reused": totals.reused,
        "uploaded_bytes": totals.uploaded_bytes,
        "epoch": totals.epoch,
        "state": state_name(totals.state),
    })
}

/// Order statistics, as the document reports them.
pub(crate) fn percentiles(stats: &Percentiles) -> Value {
    json!({
        "samples": stats.samples,
        "p50": stats.p50,
        "p95": stats.p95,
        "p99": stats.p99,
        "max": stats.max,
    })
}

/// What a session counted as it went.
#[derive(Default)]
struct Tally {
    /// Receipts presented.
    receipts: u64,
    /// Frames a backend had to upload.
    uploads: usize,
    /// Bytes those receipts uploaded.
    upload_bytes: u64,
    /// Idle presents.
    idle: u64,
    /// Presents refused while the device was draining or lost.
    unavailable: u64,
    /// Presents refused for a budget.
    backpressure: u64,
    /// Receipts stamped with an epoch other than their report's.
    stale_receipts: u64,
    /// The digest of every receipt, in order.
    digest: u64,
}

impl Tally {
    /// Count one presented report.
    fn report(&mut self, report: &RuntimeReport) {
        if report.idle {
            self.idle = self.idle.saturating_add(1);
        }
        self.uploads = self.uploads.saturating_add(report.uploads().len());
        for &FrameReceipt {
            host,
            shapes,
            upload_bytes,
            epoch,
            digest,
            ..
        } in report.receipts()
        {
            self.receipts = self.receipts.saturating_add(1);
            self.upload_bytes = self.upload_bytes.saturating_add(upload_bytes);
            if epoch != report.epoch {
                self.stale_receipts = self.stale_receipts.saturating_add(1);
            }
            // The shape count folds in byte by byte, so the digest is the same
            // whatever width `usize` has on the host that ran the session.
            let counted = shapes
                .to_le_bytes()
                .iter()
                .fold(fold(self.digest, u64::from(host.0)), |hash, &byte| {
                    fold(hash, u64::from(byte))
                });
            self.digest = fold(counted, digest);
        }
    }

    /// Count one refused present.
    fn refusal(&mut self, error: RuntimeError) {
        if error == RuntimeError::Backpressure {
            self.backpressure = self.backpressure.saturating_add(1);
        } else {
            self.unavailable = self.unavailable.saturating_add(1);
        }
    }
}

/// Run one residency session of `frames` frames from `seed`.
pub(crate) fn session(seed: u64, frames: u32) -> Value {
    let budgets = Budgets::conservative();
    let mut runtime = Runtime::new(seed, budgets);
    let mut tally = Tally {
        digest: 0xcbf2_9ce4_8422_2325,
        ..Tally::default()
    };
    // Every call the profile refuses during setup, by name: the session
    // reports them rather than running on a smaller set without saying so.
    let mut refused_setup = Vec::new();
    let mut hosts = Vec::new();
    for effect in Effect::all() {
        match runtime.add_host(HostSpec::new(effect, Canvas::new(248.0, 168.0), true)) {
            Ok(host) => hosts.push(host),
            Err(error) => refused_setup.push(format!("{}: {error:?}", effect.name())),
        }
    }
    for &host in &hosts {
        let mut input = Input::default();
        input.pointer = Some((124.0, 84.0));
        if let Err(error) = runtime.set_options(host, Options::new()) {
            refused_setup.push(format!("options: {error:?}"));
        }
        if let Err(error) = runtime.set_input(host, input) {
            refused_setup.push(format!("input: {error:?}"));
        }
    }
    runtime.set_quality(Quality::Auto);
    runtime.set_speed_milli(1_000);
    runtime.set_paused(false);
    runtime.set_reduce_motion(false);
    let (lose_at, hide_at) = (frames.div_ceil(3), frames.saturating_mul(2).div_ceil(3));
    let mut samples = match usize::try_from(frames) {
        Ok(frames) => Vec::with_capacity(frames.saturating_add(2)),
        Err(_) => Vec::new(),
    };
    let mut epochs = vec![runtime.epoch()];
    for frame in 0..frames {
        if frame == lose_at {
            runtime.drain();
            present(&mut runtime, &mut tally, &mut samples);
            runtime.lose_device();
            present(&mut runtime, &mut tally, &mut samples);
            epochs.push(runtime.recover());
        }
        if frame == hide_at {
            for &host in hosts.iter().step_by(2) {
                runtime.set_visible(host, false);
            }
        }
        if frame.saturating_add(1) == frames {
            runtime.set_reduce_motion(true);
        }
        present(&mut runtime, &mut tally, &mut samples);
    }
    let ink = runtime.ink_stats();
    json!({
        "seed": seed,
        "frames": frames,
        "hosts": hosts.len(),
        "refused_setup": refused_setup,
        "budgets": budgets_json(&budgets),
        "epochs": epochs,
        "state": state_name(runtime.state()),
        "receipts": tally.receipts,
        "uploads": tally.uploads,
        "upload_bytes": tally.upload_bytes,
        "idle": tally.idle,
        "refused_unavailable": tally.unavailable,
        "refused_backpressure": tally.backpressure,
        "stale_receipts": tally.stale_receipts,
        "digest": format!("{:016x}", tally.digest),
        "totals": totals(&runtime.totals()),
        "ink": {
            "frames": ink.frames,
            "live": ink.live,
            "lite": ink.lite,
            "stride_skips": ink.stride_skips,
        },
        "present_ms": percentiles(&Percentiles::of(&samples)),
    })
}

/// Present one frame, timing it and counting what it did.
fn present(runtime: &mut Runtime, tally: &mut Tally, samples: &mut Vec<f64>) {
    let started = Instant::now();
    let outcome = runtime.present(FRAME_MS);
    samples.push(started.elapsed().as_secs_f64() * 1000.0);
    match outcome {
        Ok(report) => tally.report(&report),
        Err(error) => tally.refusal(error),
    }
}

/// A profile's budgets, as the document reports them.
fn budgets_json(budgets: &Budgets) -> Value {
    json!({
        "max_shapes_per_frame": budgets.max_shapes_per_frame,
        "max_upload_bytes_per_frame": budgets.max_upload_bytes_per_frame,
        "max_in_flight": budgets.max_in_flight,
        "max_hosts": budgets.max_hosts,
    })
}

/// `drishti limits`: every bound the engine and the runtime declare.
pub(crate) fn limits() -> Value {
    json!({
        "ink": {
            "max_shapes": MAX_SHAPES,
            "max_beads": MAX_BEADS,
            "lite_enter_ms": LITE_ENTER_MS,
            "lite_leave_ms": LITE_LEAVE_MS,
            "max_option_ms": MAX_OPTION_MS,
            "max_option_pixels": MAX_OPTION_PIXELS,
            "max_option_amount": MAX_OPTION_AMOUNT,
            "max_count_magnitude": MAX_COUNT_MAGNITUDE,
            "max_count_decimals": MAX_COUNT_DECIMALS,
            "max_canvas": MAX_CANVAS,
        },
        "runtime": {
            "bytes_per_shape": BYTES_PER_SHAPE,
            "bytes_per_filter": BYTES_PER_FILTER,
            "conservative": budgets_json(&Budgets::conservative()),
        },
    })
}

/// A native session's report, as the document reports it.
#[cfg(feature = "ui")]
pub(crate) fn session_report(report: &drishti::native::SessionReport) -> Value {
    json!({
        "totals": totals(&report.totals),
        "frame_interval_ms": percentiles(&report.frame_interval_ms),
        "refusals": report.refusals,
        "paint_failures": report.paint_failures,
        "wall_ms": report.wall_ms,
    })
}

/// Why the native host could not run, as the document reports it.
#[cfg(feature = "ui")]
pub(crate) fn native_refusal(error: &drishti::native::NativeError) -> Value {
    json!({ "refused": error.to_string() })
}
