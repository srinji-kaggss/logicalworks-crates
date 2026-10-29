//! Logical Works brand tokens, encoded as typed Rust.
//!
//! The brand is "flat build, liquid motion": three layers (primitive values,
//! semantic roles, component usage), two themes (warm chalk light, graphite
//! dark), and nine flat colourways that swap the ink/accent pair without ever
//! adding a third colour. This module is the single source of those values for
//! the ink lab and the presentation runtime; the motion layer, the ink effects
//! and the residency budgets all read roles from here rather than literal
//! colours. Sources: `lw-tokens.json` and `docs/inklab.md` (S22).
//!
//! ```text
//! primitive --lw-p-*   raw values with a scale number; never used directly
//! semantic  --lw-c-*   roles (canvas, surface, text, accent, focus)
//! component            reads semantic roles only
//! ```

/// An 8-bit-per-channel sRGB colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Rgb {
    /// Red channel.
    pub red: u8,
    /// Green channel.
    pub green: u8,
    /// Blue channel.
    pub blue: u8,
}

impl Rgb {
    /// Build a colour from its three channels.
    ///
    /// `const` so palettes can be compile-time constants.
    #[must_use]
    pub const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }

    /// Parse `#rrggbb` or `rrggbb` into a colour.
    ///
    /// Returns [`None`] on any malformed input rather than silently zeroing;
    /// the brand rejects invented values.
    #[must_use]
    pub fn from_hex(value: &str) -> Option<Self> {
        let digits = match value.strip_prefix('#') {
            Some(rest) => rest,
            None => value,
        };
        let bytes = digits.as_bytes();
        if bytes.len() != 6 {
            return None;
        }
        let red = hex_byte(bytes[0], bytes[1])?;
        let green = hex_byte(bytes[2], bytes[3])?;
        let blue = hex_byte(bytes[4], bytes[5])?;
        Some(Self::new(red, green, blue))
    }

    /// Render as lowercase `#rrggbb`.
    #[must_use]
    pub fn hex(self) -> String {
        format!(
            "#{}",
            lgwks_std::hex::encode([self.red, self.green, self.blue])
        )
    }

    /// Render as `rgb(r, g, b)` for CSS or logs.
    #[must_use]
    pub fn css(self) -> String {
        format!("rgb({}, {}, {})", self.red, self.green, self.blue)
    }

    /// WCAG 2.2 relative luminance in `[0.0, 1.0]`.
    #[must_use]
    pub fn luminance(self) -> f64 {
        let channel = |value: u8| {
            let value = f64::from(value) / 255.0;
            if value <= 0.039_28 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(self.red) + 0.7152 * channel(self.green) + 0.0722 * channel(self.blue)
    }

    /// WCAG 2.2 contrast ratio against `other`, in `[1.0, 21.0]`.
    #[must_use]
    pub fn contrast(self, other: Self) -> f64 {
        let (high, low) = {
            let first = self.luminance();
            let second = other.luminance();
            if first >= second {
                (first, second)
            } else {
                (second, first)
            }
        };
        (high + 0.05) / (low + 0.05)
    }
}

/// Decode one hex digit, or [`None`] if it is not `0-9a-fA-F`.
fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte.wrapping_sub(b'0')),
        b'a'..=b'f' => Some(byte.wrapping_sub(b'a').wrapping_add(10)),
        b'A'..=b'F' => Some(byte.wrapping_sub(b'A').wrapping_add(10)),
        _ => None,
    }
}

/// Decode a two-digit hex byte.
fn hex_byte(high: u8, low: u8) -> Option<u8> {
    let high = hex_digit(high)?;
    let low = hex_digit(low)?;
    Some(high.wrapping_mul(16).wrapping_add(low))
}

/// Raw primitive values from the brand's `--lw-p-*` layer.
///
/// Components never read these directly; they exist so the semantic tables
/// below have one place to re-point when the palette is revised.
pub mod primitive {
    use super::Rgb;

    /// Ink 900 — the brand's primary ink.
    pub const INK_900: Rgb = Rgb::new(0x22, 0x26, 0x20);
    /// Ink 700 — secondary ink.
    pub const INK_700: Rgb = Rgb::new(0x45, 0x44, 0x3e);
    /// Ink 500 — subtle ink.
    pub const INK_500: Rgb = Rgb::new(0x62, 0x60, 0x5a);
    /// Chalk 50 — the brightest warm ground.
    pub const CHALK_50: Rgb = Rgb::new(0xfa, 0xf7, 0xee);
    /// Chalk 100 — the warm canvas.
    pub const CHALK_100: Rgb = Rgb::new(0xf5, 0xf1, 0xe8);
    /// Chalk 300 — quiet edges and wells.
    pub const CHALK_300: Rgb = Rgb::new(0xe5, 0xe0, 0xd6);
    /// Clay 400 — the lighter signal.
    pub const CLAY_400: Rgb = Rgb::new(0xe0, 0x58, 0x3a);
    /// Clay 500 — the one brand signal.
    pub const CLAY_500: Rgb = Rgb::new(0xc8, 0x40, 0x25);
    /// Clay 700 — the pressed signal.
    pub const CLAY_700: Rgb = Rgb::new(0x9a, 0x3b, 0x22);
    /// Graphite 950 — the deepest sunken surface.
    pub const GRAPHITE_950: Rgb = Rgb::new(0x10, 0x12, 0x10);
    /// Graphite 900 — the dark canvas.
    pub const GRAPHITE_900: Rgb = Rgb::new(0x16, 0x19, 0x15);
    /// Graphite 800 — the dark surface.
    pub const GRAPHITE_800: Rgb = Rgb::new(0x1f, 0x23, 0x1e);
    /// Graphite 600 — dark edges.
    pub const GRAPHITE_600: Rgb = Rgb::new(0x3a, 0x3f, 0x37);
    /// Clay 300 — the dark-theme signal.
    pub const CLAY_300: Rgb = Rgb::new(0xee, 0x6f, 0x50);
    /// Pine 600 — confirmed.
    pub const PINE_600: Rgb = Rgb::new(0x2e, 0x6b, 0x45);
    /// Amber 400 — caution.
    pub const AMBER_400: Rgb = Rgb::new(0xf2, 0xb1, 0x34);
    /// Blue 600 — focus and neutral notice.
    pub const BLUE_600: Rgb = Rgb::new(0x1a, 0x4f, 0xd6);
    /// Red 700 — error and destructive.
    pub const RED_700: Rgb = Rgb::new(0xb3, 0x26, 0x1e);
}

/// A semantic role from the brand's `--lw-c-*` layer.
///
/// Components address roles, never values, so one attribute re-themes a whole
/// surface. The list is closed for now; new roles require a light/dark pair
/// and a contrast check per the brand's accessibility rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Role {
    /// Page background.
    Canvas,
    /// Cards, fields and panels.
    Surface,
    /// Wells, table hover and skeletons.
    SurfaceSunken,
    /// Body and headings.
    Text,
    /// Secondary copy.
    TextMuted,
    /// Hints and captions.
    TextSubtle,
    /// Dividers and quiet edges.
    Border,
    /// Field edges and rules.
    BorderStrong,
    /// The one signal: primary action and active state.
    Accent,
    /// Accent pressed or hovered.
    AccentHover,
    /// Text on the accent.
    OnAccent,
    /// Inverse blocks, toasts and footers.
    Inverse,
    /// Text on inverse.
    OnInverse,
    /// Focus ring only.
    Focus,
    /// Confirmed.
    Success,
    /// Caution.
    Warning,
    /// Error and destructive.
    Danger,
    /// Neutral notice.
    Info,
}

/// Light or dark theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Theme {
    /// Warm chalk ground (the 5a primary light pair).
    Light,
    /// Graphite ground (decision D4, replacing the retired maroon night).
    Dark,
}

/// One of the brand's nine approved flat colourways (`5a`–`5i`).
///
/// Each is a pair: ink plus one accent beam on one ground. Colourways are for
/// a single campaign or partner and never mix on one page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Colourway {
    /// 5a · Clay on chalk — the primary.
    ClayOnChalk,
    /// 5b · Chalk on ink — the primary reversed.
    ChalkOnInk,
    /// 5c · Clay ground — one-colour ground.
    ClayGround,
    /// 5d · Black and red — defence-grade.
    BlackRed,
    /// 5e · Black and blue — institutional.
    BlackBlue,
    /// 5f · Navy ground — blueprint.
    NavyGround,
    /// 5g · Amber dawn — hope, light.
    AmberDawn,
    /// 5h · Sky — cool daylight.
    Sky,
    /// 5i · Mono black — stamp, fax, etch.
    MonoBlack,
}

/// The three colours a colourway swaps: ground, ink and the single accent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Paint {
    /// Background ground.
    pub ground: Rgb,
    /// Mark and text ink.
    pub ink: Rgb,
    /// The one signal beam.
    pub accent: Rgb,
}

impl Colourway {
    /// All nine colourways in brand order.
    #[must_use]
    pub const fn all() -> [Self; 9] {
        [
            Self::ClayOnChalk,
            Self::ChalkOnInk,
            Self::ClayGround,
            Self::BlackRed,
            Self::BlackBlue,
            Self::NavyGround,
            Self::AmberDawn,
            Self::Sky,
            Self::MonoBlack,
        ]
    }

    /// The colourway's short brand id (`5a`–`5i`).
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::ClayOnChalk => "5a",
            Self::ChalkOnInk => "5b",
            Self::ClayGround => "5c",
            Self::BlackRed => "5d",
            Self::BlackBlue => "5e",
            Self::NavyGround => "5f",
            Self::AmberDawn => "5g",
            Self::Sky => "5h",
            Self::MonoBlack => "5i",
        }
    }

    /// The ground, ink and accent this colourway swaps in.
    #[must_use]
    pub const fn paint(self) -> Paint {
        match self {
            Self::ClayOnChalk => Paint {
                ground: primitive::CHALK_100,
                ink: primitive::INK_900,
                accent: primitive::CLAY_500,
            },
            Self::ChalkOnInk => Paint {
                ground: primitive::INK_900,
                ink: primitive::CHALK_100,
                accent: primitive::CLAY_400,
            },
            Self::ClayGround => Paint {
                ground: primitive::CLAY_500,
                ink: primitive::CHALK_100,
                accent: primitive::INK_900,
            },
            // The source lists each colourway as ink / accent / ground.
            Self::BlackRed => Paint {
                ground: Rgb::new(0xff, 0xff, 0xff),
                ink: Rgb::new(0x0a, 0x0a, 0x0a),
                accent: Rgb::new(0xd7, 0x19, 0x20),
            },
            Self::BlackBlue => Paint {
                ground: Rgb::new(0xff, 0xff, 0xff),
                ink: Rgb::new(0x0a, 0x0a, 0x0a),
                accent: primitive::BLUE_600,
            },
            Self::NavyGround => Paint {
                ground: Rgb::new(0x0b, 0x1f, 0x3a),
                ink: primitive::CHALK_100,
                accent: Rgb::new(0xff, 0x6a, 0x3d),
            },
            Self::AmberDawn => Paint {
                ground: Rgb::new(0x1b, 0x19, 0x17),
                ink: primitive::CHALK_100,
                accent: primitive::AMBER_400,
            },
            Self::Sky => Paint {
                ground: Rgb::new(0xdf, 0xe9, 0xf2),
                ink: Rgb::new(0x14, 0x21, 0x3a),
                accent: Rgb::new(0xe8, 0x55, 0x2f),
            },
            Self::MonoBlack => Paint {
                ground: primitive::CHALK_100,
                ink: Rgb::new(0x0a, 0x0a, 0x0a),
                accent: Rgb::new(0x0a, 0x0a, 0x0a),
            },
        }
    }
}

/// A theme plus an optional colourway: the resolver components read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Palette {
    /// Light or dark ground.
    pub theme: Theme,
    /// Flat colourway pair; `ClayOnChalk` is the light default.
    pub colourway: Colourway,
}

impl Palette {
    /// The primary light palette (5a).
    #[must_use]
    pub const fn light() -> Self {
        Self {
            theme: Theme::Light,
            colourway: Colourway::ClayOnChalk,
        }
    }

    /// The graphite dark palette (decision D4).
    #[must_use]
    pub const fn dark() -> Self {
        Self {
            theme: Theme::Dark,
            colourway: Colourway::ClayOnChalk,
        }
    }

    /// Resolve a semantic role to a concrete colour.
    ///
    /// Dark mode is graphite and ignores campaign colourways; light mode lets a
    /// colourway swap the ground, ink and accent and keeps the remaining roles
    /// on the primary pair so text and status stay legible.
    #[must_use]
    pub const fn role(self, role: Role) -> Rgb {
        match self.theme {
            Theme::Dark => match role {
                Role::Canvas => primitive::GRAPHITE_900,
                Role::Surface => primitive::GRAPHITE_800,
                Role::SurfaceSunken => primitive::GRAPHITE_950,
                Role::Text | Role::BorderStrong | Role::Inverse => primitive::CHALK_100,
                Role::TextMuted => Rgb::new(0xcf, 0xcb, 0xbe),
                Role::TextSubtle => Rgb::new(0xa3, 0xa0, 0x94),
                Role::Border => primitive::GRAPHITE_600,
                Role::Accent => primitive::CLAY_300,
                Role::AccentHover => Rgb::new(0xf5, 0x8a, 0x70),
                Role::OnAccent | Role::OnInverse => primitive::GRAPHITE_900,
                Role::Focus => Rgb::new(0x8f, 0xb0, 0xff),
                Role::Success => Rgb::new(0x6f, 0xcf, 0x97),
                Role::Warning => primitive::AMBER_400,
                Role::Danger => Rgb::new(0xff, 0x8a, 0x80),
                Role::Info => Rgb::new(0x8f, 0xb0, 0xff),
            },
            Theme::Light => {
                let paint = self.colourway.paint();
                match role {
                    Role::Canvas | Role::Surface => paint.ground,
                    Role::SurfaceSunken => primitive::CHALK_300,
                    Role::Text | Role::BorderStrong | Role::Inverse => paint.ink,
                    Role::TextMuted => primitive::INK_700,
                    Role::TextSubtle => primitive::INK_500,
                    Role::Border => primitive::CHALK_300,
                    Role::Accent => paint.accent,
                    Role::AccentHover => primitive::CLAY_700,
                    Role::OnAccent => paint.ground,
                    Role::OnInverse => primitive::CHALK_100,
                    Role::Focus => primitive::BLUE_600,
                    Role::Success => primitive::PINE_600,
                    Role::Warning => primitive::AMBER_400,
                    Role::Danger => primitive::RED_700,
                    Role::Info => primitive::BLUE_600,
                }
            }
        }
    }
}

/// The brand's three type families, plus the two additions it ships.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TypeRole {
    /// League Gothic, all caps — headlines and the wordmark.
    Display,
    /// Fira Sans regular — reading.
    Text,
    /// Fira Sans medium — labels, captions and specs.
    Label,
    /// Rochester — accent and highlights only, one per view.
    Script,
    /// Fragment Mono — tabular figures and code.
    Mono,
}

impl TypeRole {
    /// The family name to request from the host text stack.
    #[must_use]
    pub const fn family(self) -> &'static str {
        match self {
            Self::Display => "League Gothic",
            Self::Text | Self::Label => "Fira Sans",
            Self::Script => "Rochester",
            Self::Mono => "Fragment Mono",
        }
    }

    /// Tracking in thousandths of an em (`+12` means `+0.012em`).
    #[must_use]
    pub const fn tracking_milli_em(self) -> i32 {
        match self {
            Self::Display => 12,
            Self::Label => 30,
            Self::Script => 0,
            Self::Text | Self::Mono => 0,
        }
    }
}

/// Motion duration tokens, in milliseconds.
pub mod duration {
    /// Press feedback.
    pub const INSTANT: u32 = 90;
    /// Hover and focus.
    pub const FAST: u32 = 160;
    /// Most transitions.
    pub const BASE: u32 = 280;
    /// Dialogs and page parts.
    pub const SLOW: u32 = 480;
    /// Brand moments only.
    pub const INK: u32 = 900;
}

/// Motion easing tokens as cubic-bezier control points `[x1, y1, x2, y2]`.
pub mod easing {
    /// Utility: hover, toggle, move.
    pub const STANDARD: [f64; 4] = [0.4, 0.0, 0.2, 1.0];
    /// Arrivals: modals, cards, menus.
    pub const SETTLE: [f64; 4] = [0.22, 1.0, 0.36, 1.0];
    /// Liquid: fills, wipes, blooms.
    pub const INK: [f64; 4] = [0.65, 0.0, 0.35, 1.0];
    /// Departures: dismiss, close.
    pub const EXIT: [f64; 4] = [0.4, 0.0, 1.0, 1.0];
}

/// The 4-pixel spacing scale.
pub const SPACE_SCALE: [u32; 10] = [4, 8, 12, 16, 24, 32, 48, 64, 96, 128];

/// Corner radii. `PILL` is a stand-in for a fully rounded edge.
pub mod radius {
    /// Square corners.
    pub const NONE: u32 = 0;
    /// Small.
    pub const SM: u32 = 2;
    /// Medium.
    pub const MD: u32 = 4;
    /// Fully rounded.
    pub const PILL: u32 = 999;
}

/// Versioned layout breakpoints, in CSS pixels.
pub mod breakpoint {
    /// Extra small.
    pub const EXTRA_SMALL: u32 = 0;
    /// Small.
    pub const SMALL: u32 = 480;
    /// Medium.
    pub const MEDIUM: u32 = 760;
    /// Large.
    pub const LARGE: u32 = 1000;
    /// Extra large.
    pub const EXTRA_LARGE: u32 = 1300;
    /// Double extra large.
    pub const DOUBLE_EXTRA_LARGE: u32 = 1760;
}

/// WCAG 2.2 conformance result for a measured pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Wcag {
    /// At least 7:1 — enhanced.
    Aaa,
    /// At least 4.5:1 normal text, or 3:1 large text.
    Aa,
    /// At least 3:1, allowed only for large text or UI.
    LargeOnly,
    /// Below every threshold.
    Fail,
}

impl Wcag {
    /// Grading for a contrast ratio. `large` is true for large text or UI
    /// components, where the floor is 3:1 rather than 4.5:1.
    #[must_use]
    pub fn grade(ratio: f64, large: bool) -> Self {
        if ratio >= 7.0 {
            Self::Aaa
        } else if ratio >= 4.5 {
            Self::Aa
        } else if large && ratio >= 3.0 {
            Self::LargeOnly
        } else {
            Self::Fail
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Colourway, Palette, Rgb, Role, Theme, Wcag};

    #[test]
    fn hex_round_trips_and_rejects_garbage() {
        let clay = Rgb::from_hex("#c84025");
        assert_eq!(clay, Some(Rgb::new(0xc8, 0x40, 0x25)));
        assert_eq!(clay.map(Rgb::hex).as_deref(), Some("#c84025"));
        assert_eq!(Rgb::from_hex("222620"), Some(Rgb::new(0x22, 0x26, 0x20)));
        for bad in ["", "#12345", "#1234567", "#gggggg", "not a colour"] {
            assert_eq!(Rgb::from_hex(bad), None, "accepted {bad:?}");
        }
    }

    #[test]
    fn brand_contrast_pairs_match_the_published_table() {
        // 5a: ink on chalk is 13.6:1 in the brand's computed table.
        let ratio = Palette::light()
            .role(Role::Text)
            .contrast(Palette::light().role(Role::Canvas));
        assert!(ratio > 13.0, "ink on chalk measured {ratio:.1}");
        // 5a accent on canvas is 4.4:1: large text or UI only, and it fails
        // the normal-text floor, which is why the brand asks for an accent
        // rule beside body copy rather than accent-coloured body text.
        let accent = Palette::light()
            .role(Role::Accent)
            .contrast(Palette::light().role(Role::Canvas));
        assert!((4.0..4.6).contains(&accent), "accent measured {accent:.2}");
        assert_eq!(Wcag::grade(accent, true), Wcag::LargeOnly);
        assert_eq!(Wcag::grade(accent, false), Wcag::Fail);
        // The dark theme's graphite canvas keeps text comfortably AAA.
        let dark = Palette::dark();
        assert!(dark.role(Role::Text).contrast(dark.role(Role::Canvas)) >= 7.0);
    }

    #[test]
    fn every_colourway_has_a_legible_ink_on_its_ground() {
        for colourway in Colourway::all() {
            let paint = colourway.paint();
            let ratio = paint.ink.contrast(paint.ground);
            // 3:1 is the brand's floor for large text and UI; 5c (clay
            // ground) sits at 4.42, so a blanket 4.5 is not a real invariant.
            assert!(
                ratio >= 3.0,
                "{} ink/ground contrast {ratio:.2} is below the 3:1 UI floor",
                colourway.id()
            );
        }
        // The two primary colourways must clear the normal-text floor.
        for primary in [Colourway::ClayOnChalk, Colourway::ChalkOnInk] {
            let paint = primary.paint();
            assert!(
                paint.ink.contrast(paint.ground) >= 4.5,
                "primary {} must clear AA for body text",
                primary.id()
            );
        }
    }

    #[test]
    fn dark_theme_is_graphite_and_light_default_is_5a() {
        assert_eq!(Palette::dark().theme, Theme::Dark);
        assert_eq!(
            Palette::dark().role(Role::Canvas),
            Rgb::new(0x16, 0x19, 0x15)
        );
        assert_eq!(
            Palette::light().role(Role::Accent),
            Rgb::new(0xc8, 0x40, 0x25)
        );
    }
}
