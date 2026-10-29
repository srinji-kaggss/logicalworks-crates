//! The native GPU host: the ink lab presented through GPUI.
//!
//! This is the `presentation/gpui.rs` seat from `docs/codebase.md`, behind the
//! `ui` feature. GPUI arrives through the estate storefront
//! (`lgwks_deps/gpui`), never as a direct edge. The host owns no business
//! state and no animation logic: every frame comes from [`crate::runtime::Runtime::present`],
//! which stays the only supported way to present an ink frame.
//!
//! * **Demand drives the frame loop.** The view asks GPUI for another frame
//!   only while the runtime reports demand. When every host has finished or
//!   the device is lost, it stops asking and the window sleeps until input
//!   arrives.
//! * **Residency is real here.** The view keeps one resident frame per host.
//!   It replaces that frame only when the runtime reports an upload, and
//!   drops all of them when the device epoch changes. A reused receipt
//!   paints the resident copy.
//! * **Refusals are visible.** Backpressure and device loss appear in the
//!   status line and in the session report. None of them is swallowed.
//!
//! What this host does not do yet: the goo filter (blur plus alpha threshold)
//! and the text turbulence. GPUI 0.2 exposes neither a blur nor a threshold
//! pass, so liquid shapes are painted flat and the text effect shows its
//! opacity ramp. Each frame's `blur` and `displacement` parameters are still
//! reported, so a later backend can be checked against them.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use lgwks_deps::gpui::{
    App, AppContext as _, Application, Bounds, Pixels, SharedString, TitlebarOptions, WindowBounds,
    WindowOptions, size,
};

use crate::runtime::{Percentiles, RuntimeError, RuntimeTotals};

mod lab;

use lab::InkLab;

/// Width of one effect tile's drawing area, in logical pixels.
const TILE_WIDTH: f64 = 248.0;

/// Height of one effect tile's drawing area, in logical pixels.
const TILE_HEIGHT: f64 = 168.0;

/// Longest frame delta fed to the engine, in milliseconds.
///
/// A window that slept, or a stalled compositor, must not teleport every
/// effect forward. The engine clamps its own accumulator at 200 ms; this
/// bound keeps a single step to a believable frame.
const MAX_FRAME_MS: f64 = 100.0;

/// The delta used for the first frame after the loop wakes, in milliseconds.
const WAKE_FRAME_MS: f64 = 1000.0 / 60.0;

/// Upper bound on retained frame-interval samples.
const MAX_SAMPLES: usize = 16_384;

/// How long a frame-limited session may go without a frame before it ends
/// with [`NativeError::NoFrames`]. The error's documentation states this
/// value; keep the two in step.
///
/// macOS sends no display-link frames to a locked session or a sleeping
/// display, so a measured run would otherwise wait forever.
const STALL_LIMIT: Duration = Duration::from_secs(10);

/// How often the stall watchdog checks.
const STALL_CHECK: Duration = Duration::from_secs(1);

/// How to launch the native ink lab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Launch {
    /// Master seed for the ink engine; one seed replays the same geometry.
    pub seed: u64,
    /// Start with reduced motion: every effect resolves to its finished frame.
    pub reduce_motion: bool,
    /// Start on the graphite dark theme instead of warm chalk.
    pub dark: bool,
    /// Quit after this many frames (presented plus idle); `None` runs until
    /// the window closes.
    pub frame_limit: Option<u64>,
}

impl Launch {
    /// The default launch: seed 1, full motion, light theme, no frame limit.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            seed: 1,
            reduce_motion: false,
            dark: false,
            frame_limit: None,
        }
    }
}

impl Default for Launch {
    /// The default launch.
    fn default() -> Self {
        Self::new()
    }
}

/// Why the native host could not run.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum NativeError {
    /// The platform refused to open the window.
    WindowOpen(String),
    /// The runtime profile refused a host at startup.
    Runtime(RuntimeError),
    /// A frame-limited session received no frame from the platform for ten
    /// seconds.
    NoFrames {
        /// Frames presented before the stall.
        presented: u64,
    },
}

impl std::fmt::Display for NativeError {
    /// A one-line explanation naming the refused step.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::WindowOpen(ref reason) => write!(formatter, "window refused: {reason}"),
            Self::Runtime(error) => write!(formatter, "runtime refused a host: {error:?}"),
            Self::NoFrames { presented } => write!(
                formatter,
                "the platform delivered no frame for {} s after {presented} presented frames; \
                 macOS sends none to a locked session or a sleeping display",
                STALL_LIMIT.as_secs()
            ),
        }
    }
}

impl std::error::Error for NativeError {}

/// What one native session measured, handed to the launcher when it ends.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct SessionReport {
    /// The runtime's cumulative counters.
    pub totals: RuntimeTotals,
    /// Intervals between consecutive demanded frames, as GPUI scheduled them.
    pub frame_interval_ms: Percentiles,
    /// Frames the runtime refused for a budget or an unavailable device.
    pub refusals: u64,
    /// Shapes the host could not tessellate and therefore did not paint.
    pub paint_failures: u64,
    /// Wall time from the first present to the report, in milliseconds.
    pub wall_ms: f64,
}

/// The launcher's completion callback, taken exactly once.
type Finisher = Rc<Cell<Option<Box<dyn FnOnce(Result<SessionReport, NativeError>)>>>>;

/// Call the completion callback if nobody has yet.
fn finish(finisher: &Finisher, outcome: Result<SessionReport, NativeError>) {
    if let Some(callback) = finisher.take() {
        callback(outcome);
    }
}

/// Open the ink lab window and run until it closes or `launch.frame_limit`
/// is reached.
///
/// `on_finish` receives the session report, or the reason the host could
/// not start. It is called exactly once, before the application quits.
/// On macOS the GPUI event loop never returns, so the report must be acted
/// on inside the callback rather than after `run`.
pub fn run(launch: Launch, on_finish: impl FnOnce(Result<SessionReport, NativeError>) + 'static) {
    let finisher: Finisher = Rc::new(Cell::new(Some(Box::new(on_finish))));
    Application::new().run(move |cx: &mut App| {
        let lab = match InkLab::new(launch, Rc::clone(&finisher)) {
            Ok(lab) => lab,
            Err(error) => {
                finish(&finisher, Err(error));
                cx.quit();
                return;
            }
        };
        let bounds = Bounds::centered(None, size(pixels(1180.0), pixels(860.0)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(SharedString::from("Drishti · ink lab")),
                    ..TitlebarOptions::default()
                }),
                ..WindowOptions::default()
            },
            move |window, cx| {
                let view = cx.new(move |cx| {
                    let mut lab = lab;
                    lab.arm_watchdog(cx);
                    lab
                });
                let handle = view.downgrade();
                // Closing the window ends the session: report, then quit.
                window.on_window_should_close(cx, move |_, cx| {
                    if let Some(view) = handle.upgrade() {
                        view.read(cx).finish();
                    }
                    cx.quit();
                    true
                });
                view
            },
        );
        match opened {
            Ok(_) => cx.activate(true),
            Err(error) => {
                finish(
                    &finisher,
                    Err(NativeError::WindowOpen(format!("{error:#}"))),
                );
                cx.quit();
            }
        }
    });
}

/// A logical-pixel length from a design-space float.
fn pixels(value: f64) -> Pixels {
    Pixels::from(value)
}

/// Narrow to the `f32` GPUI renders with, through its own pixel conversion
/// (the crate forbids `as`).
fn narrow(value: f64) -> f32 {
    f32::from(Pixels::from(value))
}
