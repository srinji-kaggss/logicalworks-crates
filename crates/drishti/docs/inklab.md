# The ink lab: flat build, liquid motion

Status: first deterministic reference core implemented (`src/design.rs`,
`src/ink_lab.rs`); no GPU backend, browser host, or product journey is
qualified. The visual specification is the Logical Works brand (source S22);
this document owns the Rust expression of it.

## Why the brand is part of the product

Drishti's vision says presentation is a derived view of host-owned meaning, and
that the normal path must be the safe, composable path. The Logical Works brand
adds the missing half of that sentence: what the derived view looks like when
the author does nothing special. Its own summary is exact — **flat build,
liquid motion**. The static system is flat: tokens, native semantics, no
decoration. The motion system is liquid: ink effects blur, fuse and flow while
moving, then always settle on the crisp static mark. "Nothing ships
mid-liquid."

That rule is why the brand belongs in drishti rather than beside it. A liquid
effect that can leave a surface blurred, mid-fade, or dependent on animation to
carry meaning violates reduced-motion equivalence (R02) and accessible
navigation (R26). Encoding the brand's own principle — every effect ends in a
static state — makes the aesthetic a correctness property rather than a coat of
paint.

## Foundations

Three token layers keep re-theming a one-attribute change:

| Layer | Type | Rule |
| --- | --- | --- |
| Primitive | `design::primitive` | Raw values (`ink-900`, `chalk-100`, `clay-500`); never used directly |
| Semantic | `design::Role` + `design::Palette` | Roles (`Canvas`, `Surface`, `Text`, `Accent`, `Focus`); themes and colourways remap these |
| Component | the ink lab and host controls | Read semantic roles only |

The palette ships two themes and nine flat colourways. Light is warm chalk;
dark is graphite (decision D4: canvas `#161915`, surface `#1f231e`, accent clay
`#ee6f50`), replacing the retired maroon night. The nine colourways (`5a`–`5i`)
each swap an ink/accent pair on one ground and never mix on a page. Contrast is
computed, not assumed: `Rgb::contrast` implements WCAG 2.2 relative luminance
and `Wcag::grade` names the result. The tests pin the published figures (ink on
chalk 13.6:1; clay on chalk 4.4:1, which is why the brand asks for an accent
rule beside body copy instead of accent-coloured body text).

Typography is three families plus a mono: League Gothic for display, Fira Sans
for text and labels, Rochester for accents only, Fragment Mono for tabular
figures and code. Motion tokens are roles too — `duration::{INSTANT, FAST,
BASE, SLOW, INK}` and `easing::{STANDARD, SETTLE, INK, EXIT}`.

## The eleven effects

The engine keeps the brand's public vocabulary. Each effect has a name from
`data-ink`, a default duration, and a resolution rule:

| Effect | Name | Moment | Default |
| --- | --- | --- | --- |
| Mark | `mark` | Hero logo | 2200 ms build, 1500 ms hold |
| Text | `text` | One display headline | 1500 ms condense, 1800 ms hold |
| Wipe | `wipe` | Page transition | 1100 rise, 700 hold, 1000 drain |
| Drops | `drops` | Ambient | continuous |
| Wobble | `wobble` | Idle edge | continuous, pointer-reactive |
| Loader | `loader` | Waits over 400 ms | continuous |
| Pour | `pour` | Liquid progress | continuous, `level` target |
| Cursor | `cursor` | Pointer trail | continuous |
| Ripple | `ripple` | Click bloom | 1100 ms per splash |
| Count | `count` | Figures | 1600 ms, 1800 ms hold |
| Reveal | `reveal` | Photo uncover | rise, hold, drain |

Motion is reserved for moments over 400 ms; utility interaction stays under
300 ms with `easing::STANDARD`. Every effect respects `prefers-reduced-motion`
by resolving to its finished frame — the name, the number, the crisp mark — and
never by removing the capability.

The engine, the mark and the API are in [ink-engine.md](ink-engine.md).
