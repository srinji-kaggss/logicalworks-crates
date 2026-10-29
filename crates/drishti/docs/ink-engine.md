# The ink engine: frames, the mark and its API

## The engine

`ink_lab::InkEngine` is a bounded set of hosts advanced by an injected clock.
It preserves the JavaScript engine's adaptive schedule:

* **Quality tiers.** `Quality::Auto` measures a frame-time EMA and drops to the
  `Lite` tier above `LITE_ENTER_MS` (26 ms) and recovers below `LITE_LEAVE_MS`
  (17.5 ms) after a hold; `Lite` and `Full` force the tier.
* **Stride.** Under the lite tier non-priority hosts advance every second
  frame; priority hosts (cursor, ripple, wobble) run every frame. Skips are
  counted, not hidden.
* **Off-screen hosts** accumulate nothing. Visibility, not mere existence,
  authorizes work.
* **Bounds.** A frame is capped at `MAX_SHAPES`; host counts are bounded at the
  runtime profile. A hostile host cannot grow a frame without limit (R08).
* **Refusals.** Malformed input is refused with a typed `InkError` before it
  touches any state, so no frame can carry non-finite geometry. A negative, NaN
  or infinite frame delta is `InvalidDelta`; a non-finite pointer or click is
  `InvalidInput`; an option outside the domain below is `InvalidOption(field)`.
  Canvas axes clamp to `[4, MAX_CANVAS]` (16,384 px), with NaN clamped to 4.
* **Reduced motion is never strided.** The frame that asks for reduced motion
  resolves every visible host, including hosts the lite tier would skip.

| Option fields | Accepted domain |
| --- | --- |
| `duration_ms`, `hold_ms`, `stagger_ms`, `auto_ms` | finite, `[0, 3,600,000]` ms (`MAX_OPTION_MS`) |
| `size`, `blur`, `displacement` | finite, `[0, 10,000]` px (`MAX_OPTION_PIXELS`) |
| `level`, `spout` | finite, `[0, 1]` |
| `amount`, `hover` | finite, `[0, 1,000]` (`MAX_OPTION_AMOUNT`) |
| `count_to`, `count_from` | finite, magnitude at most 10¹⁵ (`MAX_COUNT_MAGNITUDE`) |
| `count_decimals` | at most 12 (`MAX_COUNT_DECIMALS`) |
| `beads` | any value; capped at `MAX_BEADS` (64), not refused |

What deliberately differs from the JavaScript: time and randomness are
injected. The JS engine reads `Math.random` and the wall clock; here a single
seed plus a fixed input trace replay to the same `Frame::digest`, which is the
deterministic-simulation property the assurance contract asks for.

## The mark

The mark is fixed brand geometry: twenty-four tapered beams around one open
centre, clockwise from twelve o'clock, each longer than the last, with the
twenty-fourth and longest in clay. `BEAM_POINTS` embeds the published
four-point polygons verbatim; `beam_geometry()` derives each beam's centre,
half-width and normal once. While liquid, a beam is a line of soft circles that
fuse under the goo filter; when `crisp` reaches one, the engine emits the exact
polygons instead. The 24th beam is the accent — the sweep's leading edge — and
it is the one place the brand allows clay on an otherwise flat ground.

## What is not here

* No GPU rasterizer, goo filter, or wgpu edge. The reference core emits
  geometry and filter parameters; the accelerated backend is the residency
  obligation in `docs/gpu-runtime.md` and remains blocked under G03.
* No browser host or host text stack, so typography is specified but not
  measured on a real IME.
* No qualified product journey. The unit and simulation tests in this
  repository prove determinism, bounds and reduced-motion resolution; they are
  not host evidence.

## API

```text
InkEngine::new(seed)
engine.add_host(HostSpec { effect, canvas, visible }) -> HostId
engine.set_visible(id, bool)
engine.set_input(id, Input) -> Result<(), InkError>
engine.set_options(id, Options) -> Result<(), InkError>   // Options::validate()
engine.set_quality(Quality) · set_reduce_motion(bool) · set_paused(bool)
engine.step(dt_ms) -> Result<Vec<Render { host, frame }>, InkError>
frame.shapes() -> &[Shape] · frame.alpha · frame.blur · frame.displacement
frame.digest() -> u64
```

The engine is consumed through the residency model in `runtime`, which is the
only supported way to present a frame.
