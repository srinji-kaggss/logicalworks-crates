//! Render a deterministic preview of the ink lab as an animated SVG.
//!
//! Run `cargo run --example ink_preview`, then open `target/ink-preview.svg`.
//! The example is a real external consumer of the public API: it drives the
//! mark effect through its build and resolves to the crisp beams, exactly as
//! the brand specifies. It is a preview, not product runtime evidence.

use std::io::Write as _;

use drishti::design::Palette;
use drishti::ink_lab::{Canvas, Effect, HostSpec, InkEngine, InkError, Render, Shape};

/// Frames sampled across the mark build.
const FRAMES: u32 = 48;

/// Milliseconds between sampled frames.
const STEP_MS: f64 = 55.0;

/// Total loop duration in seconds.
const DURATION_SECONDS: f64 = 2.4;

/// Build the preview and write it to `target/ink-preview.svg`.
fn main() -> Result<(), std::io::Error> {
    let svg = build_svg().map_err(std::io::Error::other)?;
    let path = std::path::PathBuf::from("target/ink-preview.svg");
    std::fs::write(&path, &svg)?;
    let mut output = std::io::stdout();
    writeln!(
        output,
        "wrote {} ({} frames, {} bytes)",
        path.display(),
        FRAMES,
        svg.len()
    )?;
    Ok(())
}

/// Drive the mark effect and assemble the animated SVG.
fn build_svg() -> Result<String, InkError> {
    let palette = Palette::light();
    let ink = palette.role(drishti::design::Role::Text).hex();
    let accent = palette.role(drishti::design::Role::Accent).hex();
    let (width, height) = (120.0_f64, 140.0_f64);
    let mut engine = InkEngine::new(1);
    let _ = engine.add_host(HostSpec::new(
        Effect::Mark,
        Canvas::new(width, height),
        true,
    ));
    let mut frames: Vec<Vec<Shape>> = Vec::new();
    for _ in 0..FRAMES {
        frames.push(render_shapes(engine.step(STEP_MS)?.as_slice()));
    }
    let mut svg = String::from(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 120 140\" width=\"360\" height=\"420\">",
    );
    svg = format!(
        "{svg}<rect width=\"120\" height=\"140\" fill=\"{canvas}\"/>",
        canvas = palette.role(drishti::design::Role::Canvas).hex()
    );
    for (index, shapes) in (0_u32..).zip(frames.iter()) {
        let start = fraction(index);
        let end = fraction(index.saturating_add(1));
        svg = format!(
            "{svg}<g><animate attributeName=\"opacity\" dur=\"{DURATION_SECONDS}s\" \
             repeatCount=\"indefinite\" values=\"0;0;1;1;0;0\" \
             keyTimes=\"0;{start:.4};{start:.4};{end:.4};{end:.4};1\"/>"
        );
        for (shape_index, shape) in shapes.iter().enumerate() {
            let colour = if shape_index == 23 { &accent } else { &ink };
            svg.push_str(&shape_markup(shape, colour));
        }
        svg = format!("{svg}</g>");
    }
    svg = format!("{svg}</svg>");
    Ok(svg)
}

/// The first render's shapes, or an empty frame.
fn render_shapes(renders: &[Render]) -> Vec<Shape> {
    match renders.first() {
        Some(render) => render.frame.shapes().to_vec(),
        None => Vec::new(),
    }
}

/// A frame's start fraction in `[0, 1]`; `FRAMES` is a non-zero constant.
fn fraction(index: u32) -> f64 {
    f64::from(index) / f64::from(FRAMES)
}

/// Markup for one shape, or empty for a variant this preview does not draw.
fn shape_markup(shape: &Shape, colour: &str) -> String {
    match *shape {
        Shape::Circle {
            centre_x,
            centre_y,
            radius,
        } => format!(
            "<circle cx=\"{centre_x:.2}\" cy=\"{centre_y:.2}\" r=\"{radius:.2}\" fill=\"{colour}\"/>"
        ),
        Shape::Ellipse {
            centre_x,
            centre_y,
            radius_x,
            radius_y,
        } => format!(
            "<ellipse cx=\"{centre_x:.2}\" cy=\"{centre_y:.2}\" rx=\"{radius_x:.2}\" \
             ry=\"{radius_y:.2}\" fill=\"{colour}\"/>"
        ),
        Shape::Quad { points } => format!(
            "<polygon points=\"{:.2},{:.2} {:.2},{:.2} {:.2},{:.2} {:.2},{:.2}\" fill=\"{colour}\"/>",
            points[0][0],
            points[0][1],
            points[1][0],
            points[1][1],
            points[2][0],
            points[2][1],
            points[3][0],
            points[3][1]
        ),
        Shape::Rect {
            left,
            top,
            right,
            bottom,
        } => format!(
            "<rect x=\"{left:.2}\" y=\"{top:.2}\" width=\"{:.2}\" height=\"{:.2}\" fill=\"{colour}\"/>",
            right - left,
            bottom - top
        ),
        _ => String::new(),
    }
}
