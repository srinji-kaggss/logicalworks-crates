//! `drishti tokens`: the brand as data. Every primitive, every semantic role in
//! both themes and every colourway with its WCAG grade, the type families, the
//! motion tokens and the layout scales, plus the same set as CSS custom
//! properties so a web surface reads the values the native host paints with.

use drishti::design::primitive::{
    AMBER_400, BLUE_600, CHALK_50, CHALK_100, CHALK_300, CLAY_300, CLAY_400, CLAY_500, CLAY_700,
    GRAPHITE_600, GRAPHITE_800, GRAPHITE_900, GRAPHITE_950, INK_500, INK_700, INK_900, PINE_600,
    RED_700,
};
use drishti::design::{
    Colourway, Paint, Palette, Rgb, Role, SPACE_SCALE, Theme, TypeRole, Wcag, breakpoint, duration,
    easing, radius,
};
use lgwks_std::json::{Value, json};

/// The primitives, by their `--lw-p-*` names.
const PRIMITIVES: [(&str, Rgb); 18] = [
    ("ink-900", INK_900),
    ("ink-700", INK_700),
    ("ink-500", INK_500),
    ("chalk-50", CHALK_50),
    ("chalk-100", CHALK_100),
    ("chalk-300", CHALK_300),
    ("clay-300", CLAY_300),
    ("clay-400", CLAY_400),
    ("clay-500", CLAY_500),
    ("clay-700", CLAY_700),
    ("graphite-950", GRAPHITE_950),
    ("graphite-900", GRAPHITE_900),
    ("graphite-800", GRAPHITE_800),
    ("graphite-600", GRAPHITE_600),
    ("pine-600", PINE_600),
    ("amber-400", AMBER_400),
    ("blue-600", BLUE_600),
    ("red-700", RED_700),
];

/// The semantic roles, by their `--lw-c-*` names.
const ROLES: [(&str, Role); 18] = [
    ("canvas", Role::Canvas),
    ("surface", Role::Surface),
    ("surface-sunken", Role::SurfaceSunken),
    ("text", Role::Text),
    ("text-muted", Role::TextMuted),
    ("text-subtle", Role::TextSubtle),
    ("border", Role::Border),
    ("border-strong", Role::BorderStrong),
    ("accent", Role::Accent),
    ("accent-hover", Role::AccentHover),
    ("on-accent", Role::OnAccent),
    ("inverse", Role::Inverse),
    ("on-inverse", Role::OnInverse),
    ("focus", Role::Focus),
    ("success", Role::Success),
    ("warning", Role::Warning),
    ("danger", Role::Danger),
    ("info", Role::Info),
];

/// The type roles, by name.
const TYPE_ROLES: [(&str, TypeRole); 5] = [
    ("display", TypeRole::Display),
    ("text", TypeRole::Text),
    ("label", TypeRole::Label),
    ("script", TypeRole::Script),
    ("mono", TypeRole::Mono),
];

/// A WCAG grade's name.
const fn grade_name(grade: Wcag) -> &'static str {
    match grade {
        Wcag::Aaa => "AAA",
        Wcag::Aa => "AA",
        Wcag::LargeOnly => "large-only",
        _ => "fail",
    }
}

/// A theme's name.
const fn theme_name(theme: Theme) -> &'static str {
    match theme {
        Theme::Dark => "dark",
        _ => "light",
    }
}

/// One colour as hex and CSS, with its relative luminance.
fn colour(rgb: Rgb) -> Value {
    json!({ "hex": rgb.hex(), "css": rgb.css(), "luminance": rgb.luminance() })
}

/// A measured pair: its ratio and its grade for body text and for large text.
fn pair(foreground: Rgb, background: Rgb) -> Value {
    let ratio = foreground.contrast(background);
    json!({
        "ratio": ratio,
        "text": grade_name(Wcag::grade(ratio, false)),
        "large": grade_name(Wcag::grade(ratio, true)),
    })
}

/// Every role in one palette, and the pairs the brand's contrast table names.
fn palette(palette: Palette) -> Value {
    let roles: Vec<Value> = ROLES
        .iter()
        .map(|&(name, role)| json!({ "role": name, "colour": colour(palette.role(role)) }))
        .collect();
    let canvas = palette.role(Role::Canvas);
    json!({
        "theme": theme_name(palette.theme),
        "colourway": palette.colourway.id(),
        "roles": roles,
        "contrast": {
            "text-on-canvas": pair(palette.role(Role::Text), canvas),
            "muted-on-canvas": pair(palette.role(Role::TextMuted), canvas),
            "accent-on-canvas": pair(palette.role(Role::Accent), canvas),
            "on-accent": pair(palette.role(Role::OnAccent), palette.role(Role::Accent)),
        },
    })
}

/// One colourway's three swapped colours and how they read.
fn colourway(way: Colourway) -> Value {
    let Paint {
        ground,
        ink,
        accent,
        ..
    } = way.paint();
    json!({
        "id": way.id(),
        "ground": colour(ground),
        "ink": colour(ink),
        "accent": colour(accent),
        "ink-on-ground": pair(ink, ground),
        "accent-on-ground": pair(accent, ground),
    })
}

/// The CSS custom properties: primitives, then the light roles, then the dark
/// roles under `[data-theme="dark"]`.
fn css() -> String {
    let primitives = PRIMITIVES
        .iter()
        .map(|&(name, rgb)| format!("  --lw-p-{name}: {};\n", rgb.hex()));
    let roles = |palette: Palette| {
        ROLES
            .iter()
            .map(move |&(name, role)| format!("  --lw-c-{name}: {};\n", palette.role(role).hex()))
    };
    let light: String = primitives.chain(roles(Palette::light())).collect();
    let dark: String = roles(Palette::dark()).collect();
    format!(":root {{\n{light}}}\n[data-theme=\"dark\"] {{\n{dark}}}\n")
}

/// The whole token set.
pub(crate) fn document() -> Value {
    let primitives: Vec<Value> = PRIMITIVES
        .iter()
        .map(|&(name, rgb)| {
            // Every primitive round-trips through the parser the brand file is
            // read with, so the export and the parser cannot drift apart.
            let parsed = Rgb::from_hex(&rgb.hex()) == Some(Rgb::new(rgb.red, rgb.green, rgb.blue));
            json!({ "name": name, "colour": colour(rgb), "round_trips": parsed })
        })
        .collect();
    let type_roles: Vec<Value> = TYPE_ROLES
        .iter()
        .map(|&(name, role)| {
            json!({
                "role": name,
                "family": role.family(),
                "tracking_milli_em": role.tracking_milli_em(),
            })
        })
        .collect();
    json!({
        "primitives": primitives,
        "themes": [palette(Palette::light()), palette(Palette::dark())],
        "colourways": Colourway::all().map(colourway),
        "type": type_roles,
        "duration_ms": {
            "instant": duration::INSTANT,
            "fast": duration::FAST,
            "base": duration::BASE,
            "slow": duration::SLOW,
            "ink": duration::INK,
        },
        "easing": {
            "standard": easing::STANDARD,
            "settle": easing::SETTLE,
            "ink": easing::INK,
            "exit": easing::EXIT,
        },
        "space": SPACE_SCALE,
        "radius": {
            "none": radius::NONE,
            "sm": radius::SM,
            "md": radius::MD,
            "pill": radius::PILL,
        },
        "breakpoint": {
            "xs": breakpoint::EXTRA_SMALL,
            "sm": breakpoint::SMALL,
            "md": breakpoint::MEDIUM,
            "lg": breakpoint::LARGE,
            "xl": breakpoint::EXTRA_LARGE,
            "2xl": breakpoint::DOUBLE_EXTRA_LARGE,
        },
        "css": css(),
    })
}
