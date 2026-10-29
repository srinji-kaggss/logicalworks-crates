//! `drishti ink`: one effect stepped on its own seeded engine, reported as the
//! digest a replay must reproduce, the shape counts against their bounds, the
//! count label and the engine's own counters.

use drishti::design::Palette;
use drishti::design::Role;
use drishti::ink_lab::{
    Canvas, Effect, HostSpec, InkEngine, InkError, Input, MAX_BEADS, MAX_SHAPES, Options, Quality,
};
use lgwks_std::json::{Value, json};

/// The canvas every effect is stepped on: one tile of the ink lab, in CSS
/// pixels.
const TILE: (f64, f64) = (248.0, 168.0);

/// One 60 Hz frame, in milliseconds.
pub(crate) const FRAME_MS: f64 = 1000.0 / 60.0;

/// The digest seed: FNV-1a's offset basis.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// Fold `value` into an FNV-1a digest.
pub(crate) const fn fold(digest: u64, value: u64) -> u64 {
    (digest ^ value).wrapping_mul(0x0100_0000_01b3)
}

/// The options the command runs an effect with: the brand defaults, the
/// palette's ink and accent, and a count that shows grouping and a prefix.
fn options(effect: Effect) -> Options {
    let palette = Palette::light();
    let mut options = Options::new();
    options.ink = Some(palette.role(Role::Text));
    options.accent = Some(palette.role(Role::Accent));
    if effect == Effect::Count {
        options.count_from = Some(0.0);
        options.count_to = Some(12_480.5);
        options.count_decimals = Some(1);
        options.count_prefix = Some("$");
    }
    if effect == Effect::Cursor {
        options.beads = Some(MAX_BEADS);
    }
    options
}

/// A refused call, as the document reports it.
fn refused(effect: Effect, stage: &str, error: InkError) -> Value {
    json!({ "effect": effect.name(), "refused": { "stage": stage, "error": error.to_string() } })
}

/// Step `effect` for `frames` frames from `seed`.
pub(crate) fn run(effect: Effect, seed: u64, frames: u32, reduce_motion: bool) -> Value {
    let mut engine = InkEngine::new(seed);
    let canvas = Canvas::new(TILE.0, TILE.1);
    let host = engine.add_host(HostSpec::new(effect, canvas, false));
    let options = options(effect);
    if let Err(error) = options.validate() {
        return refused(effect, "options", error);
    }
    if let Err(error) = engine.set_options(host, options) {
        return refused(effect, "options", error);
    }
    // A pointer at the canvas centre and one click there, so the effects that
    // answer input (wobble, cursor, ripple) have input to answer.
    let mut input = Input::default();
    input.pointer = Some((canvas.width * 0.5, canvas.height * 0.5));
    input.click = input.pointer;
    if let Err(error) = engine.set_input(host, input) {
        return refused(effect, "input", error);
    }
    engine.set_quality(Quality::Auto);
    engine.set_speed_milli(1_000);
    engine.set_paused(false);
    engine.set_reduce_motion(reduce_motion);
    engine.set_visible(host, true);
    let (mut digest, mut rendered, mut peak) = (FNV_OFFSET, 0_u64, 0_usize);
    let mut label = None;
    for _ in 0..frames {
        let renders = match engine.step(FRAME_MS) {
            Ok(renders) => renders,
            Err(error) => return refused(effect, "step", error),
        };
        for render in &renders {
            let frame = &render.frame;
            digest = fold(digest, frame.digest());
            rendered = rendered.saturating_add(1);
            peak = peak.max(frame.shapes().len());
            if let Some(text) = frame.label() {
                label = Some(text.to_owned());
            }
        }
    }
    let stats = engine.stats();
    json!({
        "effect": effect.name(),
        "rendered": rendered,
        "digest": format!("{digest:016x}"),
        "peak_shapes": peak,
        "within_bound": peak <= MAX_SHAPES,
        "label": label,
        "stats": {
            "frames": stats.frames,
            "live": stats.live,
            "lite": stats.lite,
            "ema_ms": stats.ema_ms,
            "stride_skips": stats.stride_skips,
        },
    })
}
