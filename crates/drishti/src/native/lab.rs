//! The ink lab window: one tile per effect, the runtime as its only source of
//! frames, and the controls that drive motion, theme, quality and the device.

mod paint;

use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

use lgwks_deps::gpui::{
    Bounds, ClickEvent, ContentMask, Context, InteractiveElement as _, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, ParentElement as _, Pixels, Point, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, Window, canvas, div,
};

use crate::design::{Palette, Role, TypeRole};
use crate::ink_lab::{Canvas, Effect, Frame, HostId, HostSpec, Input, Options, Quality, Shape};
use crate::runtime::{Budgets, DeviceState, Percentiles, Runtime, RuntimeError};

use self::paint::{Placement, Scene, colour, design_box};
use super::{
    Finisher, Launch, MAX_FRAME_MS, MAX_SAMPLES, NativeError, STALL_CHECK, STALL_LIMIT,
    SessionReport, TILE_HEIGHT, TILE_WIDTH, WAKE_FRAME_MS, finish, narrow, pixels,
};

/// One effect tile: an ink host plus the input state its pointer produced.
struct Tile {
    /// The effect this tile runs.
    effect: Effect,
    /// The runtime host.
    host: HostId,
    /// The effect's design box, in its own coordinates.
    design: (f64, f64),
    /// Pointer position in design coordinates, while inside the tile.
    pointer: Option<(f64, f64)>,
    /// A click waiting for the next present, in design coordinates.
    click: Option<(f64, f64)>,
    /// Where the tile's canvas was last laid out, for pointer mapping.
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl Tile {
    /// The input the engine should see for this tile.
    const fn input(&self) -> Input {
        Input {
            pointer: self.pointer,
            click: self.click,
        }
    }
}

/// A host's device-resident frame.
struct Resident {
    /// The host.
    host: HostId,
    /// The last uploaded frame.
    frame: Frame,
}

/// The ink lab view: eleven effect tiles over one residency runtime.
pub(super) struct InkLab {
    /// The residency runtime; the only source of frames.
    runtime: Runtime,
    /// One tile per effect, in brand order.
    tiles: Vec<Tile>,
    /// Device-resident frames, one per host that has uploaded.
    resident: Vec<Resident>,
    /// The epoch the resident frames belong to.
    resident_epoch: u64,
    /// The active theme and colourway.
    palette: Palette,
    /// Whether reduced motion is on.
    reduce_motion: bool,
    /// The active quality mode.
    quality: Quality,
    /// When the previous demanded frame was presented.
    last_tick: Option<Instant>,
    /// When the first frame was presented.
    started: Option<Instant>,
    /// Measured intervals between demanded frames, bounded at `MAX_SAMPLES`.
    samples: Vec<f64>,
    /// Refused presents.
    refusals: u64,
    /// The most recent refusal, shown until a frame is presented.
    last_refusal: Option<RuntimeError>,
    /// Shapes that failed to tessellate, counted by the paint closures.
    paint_failures: Rc<Cell<u64>>,
    /// Quit after this many frames.
    frame_limit: Option<u64>,
    /// When the platform last asked for a frame.
    last_frame_at: Instant,
    /// The stall watchdog for frame-limited sessions; dropping the view
    /// cancels it.
    watchdog: Option<Task<()>>,
    /// The launcher's completion callback.
    finisher: Finisher,
}

impl InkLab {
    /// Build the lab: one host per effect under the conservative profile.
    pub(super) fn new(launch: Launch, finisher: Finisher) -> Result<Self, NativeError> {
        let mut runtime = Runtime::new(launch.seed, Budgets::conservative());
        let mut tiles = Vec::with_capacity(Effect::all().len());
        for effect in Effect::all() {
            let design = design_box(effect);
            let host = runtime
                .add_host(HostSpec::new(effect, Canvas::new(design.0, design.1), true))
                .map_err(NativeError::Runtime)?;
            // The ripple splashes on its own so the tile is alive before the
            // first click; everything else keeps its brand defaults.
            if effect == Effect::Ripple {
                runtime
                    .set_options(
                        host,
                        Options {
                            auto_ms: Some(1400.0),
                            ..Options::new()
                        },
                    )
                    .map_err(NativeError::Runtime)?;
            }
            tiles.push(Tile {
                effect,
                host,
                design,
                pointer: None,
                click: None,
                bounds: Rc::new(Cell::new(None)),
            });
        }
        runtime.set_reduce_motion(launch.reduce_motion);
        Ok(Self {
            resident_epoch: runtime.epoch(),
            runtime,
            tiles,
            resident: Vec::with_capacity(Effect::all().len()),
            palette: if launch.dark {
                Palette::dark()
            } else {
                Palette::light()
            },
            reduce_motion: launch.reduce_motion,
            quality: Quality::Auto,
            last_tick: None,
            started: None,
            samples: Vec::with_capacity(1024),
            refusals: 0,
            last_refusal: None,
            paint_failures: Rc::new(Cell::new(0)),
            frame_limit: launch.frame_limit,
            last_frame_at: Instant::now(),
            watchdog: None,
            finisher,
        })
    }

    /// In a frame-limited session, end with [`NativeError::NoFrames`] when the
    /// platform stops delivering frames, instead of waiting forever.
    pub(super) fn arm_watchdog(&mut self, cx: &mut Context<Self>) {
        if self.frame_limit.is_none() {
            return;
        }
        self.watchdog = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(STALL_CHECK).await;
                // `Ok(true)`: the session stalled and `end_if_stalled` handed over
                // `NoFrames`. `Err`: the lab was released with its window, and its
                // `Drop` handed over the report. Either way the outcome is
                // delivered, so the watchdog has nothing left to watch.
                let outcome = this.update(cx, |lab, cx| lab.end_if_stalled(cx));
                if !matches!(outcome, Ok(false)) {
                    break;
                }
            }
        }));
    }

    /// End the session if no frame arrived within [`STALL_LIMIT`].
    fn end_if_stalled(&self, cx: &mut Context<Self>) -> bool {
        let waited = Instant::now().saturating_duration_since(self.last_frame_at);
        if waited < STALL_LIMIT {
            return false;
        }
        finish(
            &self.finisher,
            Err(NativeError::NoFrames {
                presented: self.runtime.totals().frames_presented,
            }),
        );
        cx.quit();
        true
    }

    /// Present one frame and report whether the runtime still has demand.
    fn tick(&mut self) -> bool {
        let now = Instant::now();
        self.last_frame_at = now;
        if self.started.is_none() {
            self.started = Some(now);
        }
        let dt_ms = match self.last_tick {
            Some(previous) => {
                let interval = now.saturating_duration_since(previous).as_secs_f64() * 1000.0;
                if self.samples.len() < MAX_SAMPLES {
                    self.samples.push(interval);
                }
                interval.min(MAX_FRAME_MS)
            }
            None => WAKE_FRAME_MS,
        };
        let demand = match self.runtime.present(dt_ms) {
            Ok(report) => {
                self.last_refusal = None;
                if report.epoch != self.resident_epoch {
                    self.resident.clear();
                    self.resident_epoch = report.epoch;
                }
                for upload in report.uploads() {
                    match self
                        .resident
                        .iter_mut()
                        .find(|resident| resident.host == upload.host)
                    {
                        Some(resident) => resident.frame = upload.frame.clone(),
                        None => self.resident.push(Resident {
                            host: upload.host,
                            frame: upload.frame.clone(),
                        }),
                    }
                }
                !report.idle
            }
            Err(error) => {
                self.refusals = self.refusals.saturating_add(1);
                self.last_refusal = Some(error);
                // Backpressure leaves demand standing; a lost or draining
                // device has none until it recovers.
                error == RuntimeError::Backpressure
            }
        };
        // A click is one event, not a held state.
        let released: Vec<(HostId, Input)> = self
            .tiles
            .iter_mut()
            .filter_map(|tile| tile.click.take().map(|_| (tile.host, tile.input())))
            .collect();
        for (host, input) in released {
            self.deliver(host, input);
        }
        self.last_tick = if demand { Some(now) } else { None };
        demand
    }

    /// Hand input to the runtime; a refusal is counted and shown, never dropped.
    fn deliver(&mut self, host: HostId, input: Input) {
        if let Err(error) = self.runtime.set_input(host, input) {
            self.refusals = self.refusals.saturating_add(1);
            self.last_refusal = Some(error);
        }
    }

    /// Whether the frame limit, if any, has been reached. With a limit set,
    /// a session that runs out of demand also ends: nothing more would be
    /// presented until input arrives.
    fn limit_reached(&self, demand: bool) -> bool {
        self.frame_limit.is_some_and(|limit| {
            let totals = self.runtime.totals();
            !demand || totals.frames_presented.saturating_add(totals.idle_frames) >= limit
        })
    }

    /// The session report as of now.
    fn report(&self) -> SessionReport {
        SessionReport {
            totals: self.runtime.totals(),
            frame_interval_ms: Percentiles::of(&self.samples),
            refusals: self.refusals,
            paint_failures: self.paint_failures.get(),
            // A session that never presented a frame has run for no time.
            wall_ms: match self.started {
                Some(started) => {
                    Instant::now()
                        .saturating_duration_since(started)
                        .as_secs_f64()
                        * 1000.0
                }
                None => 0.0,
            },
        }
    }

    /// Hand the report to the launcher, once.
    pub(super) fn finish(&self) {
        finish(&self.finisher, Ok(self.report()));
    }

    /// Map a window position into a tile's design coordinates.
    fn local(&self, index: usize, position: Point<Pixels>) -> Option<(f64, f64)> {
        let tile = self.tiles.get(index)?;
        let bounds = tile.bounds.get()?;
        Some(Placement::new(bounds, tile.design).to_design(position))
    }

    /// The pointer moved over tile `index`.
    fn pointer_moved(&mut self, index: usize, position: Point<Pixels>, cx: &mut Context<Self>) {
        let local = self.local(index, position);
        if let Some(tile) = self.tiles.get_mut(index) {
            tile.pointer = local;
            let (host, input) = (tile.host, tile.input());
            self.deliver(host, input);
            cx.notify();
        }
    }

    /// The pointer left tile `index`.
    fn pointer_left(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(tile) = self.tiles.get_mut(index) {
            tile.pointer = None;
            let (host, input) = (tile.host, tile.input());
            self.deliver(host, input);
            cx.notify();
        }
    }

    /// Tile `index` was clicked.
    fn clicked(&mut self, index: usize, position: Point<Pixels>, cx: &mut Context<Self>) {
        let local = self.local(index, position);
        if let Some(tile) = self.tiles.get_mut(index) {
            tile.click = local;
            let (host, input) = (tile.host, tile.input());
            self.deliver(host, input);
            cx.notify();
        }
    }

    /// Toggle reduced motion.
    fn toggle_motion(&mut self, cx: &mut Context<Self>) {
        self.reduce_motion = !self.reduce_motion;
        self.runtime.set_reduce_motion(self.reduce_motion);
        cx.notify();
    }

    /// Toggle the light and graphite themes.
    fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        self.palette = if self.palette == Palette::dark() {
            Palette::light()
        } else {
            Palette::dark()
        };
        cx.notify();
    }

    /// Cycle auto, lite and full quality.
    fn cycle_quality(&mut self, cx: &mut Context<Self>) {
        self.quality = match self.quality {
            Quality::Auto => Quality::Lite,
            Quality::Lite => Quality::Full,
            Quality::Full => Quality::Auto,
        };
        self.runtime.set_quality(self.quality);
        cx.notify();
    }

    /// Lose the device, or recover it with a fresh epoch.
    fn toggle_device(&mut self, cx: &mut Context<Self>) {
        if self.runtime.state() == DeviceState::Ready {
            self.runtime.lose_device();
        } else {
            self.resident_epoch = self.runtime.recover();
        }
        // Frames resident on a lost device are gone with it, and a recovered
        // device starts empty.
        self.resident.clear();
        cx.notify();
    }

    /// The resident frame for a host, if it has uploaded this epoch.
    fn resident(&self, host: HostId) -> Option<&Frame> {
        self.resident
            .iter()
            .find(|resident| resident.host == host)
            .map(|resident| &resident.frame)
    }

    /// The status line under the title.
    fn status(&self) -> String {
        let totals = self.runtime.totals();
        let stats = self.runtime.ink_stats();
        let device = match totals.state {
            DeviceState::Ready => "ready",
            DeviceState::Draining => "draining",
            DeviceState::Lost => "lost",
        };
        let refusal = match self.last_refusal {
            Some(error) => format!(" · refused: {error:?}"),
            None => String::new(),
        };
        format!(
            "device {device} · epoch {} · presented {} · idle {} · uploaded {} B · reused {} · \
             live {} · {} · stride skips {}{refusal}",
            totals.epoch,
            totals.frames_presented,
            totals.idle_frames,
            totals.uploaded_bytes,
            totals.reused,
            stats.live,
            if stats.lite { "lite" } else { "full" },
            stats.stride_skips,
        )
    }

    /// One control button.
    fn button(
        &self,
        id: &'static str,
        label: &'static str,
        cx: &mut Context<Self>,
        action: fn(&mut Self, &mut Context<Self>),
    ) -> impl IntoElement + use<> {
        let palette = self.palette;
        div()
            .id(id)
            .px(pixels(12.0))
            .py(pixels(6.0))
            .rounded(pixels(4.0))
            .border_1()
            .border_color(colour(palette.role(Role::BorderStrong), 1.0))
            .text_color(colour(palette.role(Role::Text), 1.0))
            .font_family(TypeRole::Label.family())
            .text_size(pixels(13.0))
            .child(label)
            .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| action(this, cx)))
    }

    /// One effect tile.
    fn tile(&self, index: usize, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let tile = self.tiles.get(index)?;
        let palette = self.palette;
        let frame = self.resident(tile.host);
        let ink = palette.role(Role::Text);
        let accent = palette.role(Role::Accent);
        let effect = tile.effect;
        let design = tile.design;
        let failures = Rc::clone(&self.paint_failures);
        let layout = Rc::clone(&tile.bounds);
        // A host with no resident frame this epoch paints an empty, opaque tile.
        let (shapes, alpha, label): (Vec<Shape>, f64, Option<String>) = match frame {
            Some(frame) => (
                frame.shapes().to_vec(),
                frame.alpha,
                frame.label().map(String::from),
            ),
            None => (Vec::new(), 1.0, None),
        };
        let surface = canvas(
            move |bounds, _, _| layout.set(Some(bounds)),
            move |bounds, (), window, _| {
                let scene = Scene {
                    shapes: &shapes,
                    effect,
                    alpha,
                    ink,
                    accent,
                    placement: Placement::new(bounds, design),
                };
                let failed = window
                    .with_content_mask(Some(ContentMask { bounds }), |window| scene.paint(window));
                failures.set(failures.get().saturating_add(failed));
            },
        )
        .absolute()
        .size_full();
        let overlay = match effect {
            Effect::Text => Some(
                div()
                    .font_family(TypeRole::Display.family())
                    .text_size(pixels(46.0))
                    .text_color(colour(ink, 1.0))
                    .opacity(narrow(alpha))
                    .child("FLAT BUILD"),
            ),
            Effect::Count => label.map(|text| {
                div()
                    .font_family(TypeRole::Mono.family())
                    .text_size(pixels(40.0))
                    .text_color(colour(ink, 1.0))
                    .child(text)
            }),
            _ => None,
        };
        let mut stage = div()
            .relative()
            .w(pixels(TILE_WIDTH))
            .h(pixels(TILE_HEIGHT))
            .flex()
            .items_center()
            .justify_center()
            .child(surface);
        if let Some(overlay) = overlay {
            stage = stage.child(overlay);
        }
        Some(
            div()
                .id(SharedString::from(effect.name()))
                .flex()
                .flex_col()
                .gap(pixels(8.0))
                .p(pixels(12.0))
                .rounded(pixels(4.0))
                .bg(colour(palette.role(Role::Surface), 1.0))
                .border_1()
                .border_color(colour(palette.role(Role::Border), 1.0))
                .child(stage)
                .child(
                    div()
                        .font_family(TypeRole::Label.family())
                        .text_size(pixels(12.0))
                        .text_color(colour(palette.role(Role::TextMuted), 1.0))
                        .child(effect.name()),
                )
                .on_mouse_move(
                    cx.listener(move |this, event: &MouseMoveEvent, _window, cx| {
                        this.pointer_moved(index, event.position, cx);
                    }),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                        this.clicked(index, event.position, cx);
                    }),
                )
                .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
                    if !*hovered {
                        this.pointer_left(index, cx);
                    }
                })),
        )
    }
}

impl Render for InkLab {
    /// Present a frame, schedule the next only while there is demand, and
    /// lay out the lab.
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let demand = self.tick();
        if self.limit_reached(demand) {
            self.finish();
            cx.quit();
        } else if demand {
            window.request_animation_frame();
        }
        let palette = self.palette;
        let motion = if self.reduce_motion {
            "Motion: reduced"
        } else {
            "Motion: full"
        };
        let theme = if palette == Palette::dark() {
            "Theme: graphite"
        } else {
            "Theme: chalk"
        };
        let quality = match self.quality {
            Quality::Auto => "Quality: auto",
            Quality::Lite => "Quality: lite",
            Quality::Full => "Quality: full",
        };
        let device = if self.runtime.state() == DeviceState::Ready {
            "Lose device"
        } else {
            "Recover device"
        };
        let controls = div()
            .flex()
            .gap(pixels(8.0))
            .child(self.button("motion", motion, cx, Self::toggle_motion))
            .child(self.button("theme", theme, cx, Self::toggle_theme))
            .child(self.button("quality", quality, cx, Self::cycle_quality))
            .child(self.button("device", device, cx, Self::toggle_device));
        let tiles: Vec<_> = (0..self.tiles.len())
            .filter_map(|index| self.tile(index, cx))
            .collect();
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(pixels(16.0))
            .p(pixels(24.0))
            .bg(colour(palette.role(Role::Canvas), 1.0))
            .text_color(colour(palette.role(Role::Text), 1.0))
            .child(
                div()
                    .font_family(TypeRole::Display.family())
                    .text_size(pixels(34.0))
                    .child("DRISHTI · INK LAB"),
            )
            .child(
                div()
                    .font_family(TypeRole::Mono.family())
                    .text_size(pixels(12.0))
                    .text_color(colour(palette.role(Role::TextMuted), 1.0))
                    .child(self.status()),
            )
            .child(controls)
            .child(div().flex().flex_wrap().gap(pixels(16.0)).children(tiles))
    }
}

impl Drop for InkLab {
    /// A session that ends any other way still reports what it measured.
    fn drop(&mut self) {
        self.finish();
    }
}
