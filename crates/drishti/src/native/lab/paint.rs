//! Painting: where a tile sits in its bounds, and how a frame becomes GPUI
//! paths and quads.

use lgwks_deps::gpui::{self, Bounds, Hsla, PathBuilder, Pixels, Point, Window, point, size};

use crate::design::Rgb;
use crate::ink_lab::{Effect, Shape};
use crate::native::{TILE_HEIGHT, TILE_WIDTH, narrow, pixels};

/// Where a tile's design box sits inside its laid-out bounds.
#[derive(Clone, Copy, Debug)]
pub(super) struct Placement {
    /// Left edge of the design box in window pixels.
    left: f64,
    /// Top edge of the design box in window pixels.
    top: f64,
    /// Window pixels per design unit.
    scale: f64,
}

impl Placement {
    /// Fit `design` inside `bounds`, preserving its aspect ratio and centring
    /// it.
    pub(super) fn new(bounds: Bounds<Pixels>, design: (f64, f64)) -> Self {
        let width = f64::from(bounds.size.width);
        let height = f64::from(bounds.size.height);
        let scale = (width / design.0).min(height / design.1);
        Self {
            left: f64::from(bounds.origin.x) + (width - design.0 * scale) * 0.5,
            top: f64::from(bounds.origin.y) + (height - design.1 * scale) * 0.5,
            scale,
        }
    }

    /// A design-space point in window pixels.
    pub(super) fn point(self, design_x: f64, design_y: f64) -> Point<Pixels> {
        point(
            pixels(self.left + design_x * self.scale),
            pixels(self.top + design_y * self.scale),
        )
    }

    /// A window point in design space.
    pub(super) fn to_design(self, position: Point<Pixels>) -> (f64, f64) {
        (
            (f64::from(position.x) - self.left) / self.scale,
            (f64::from(position.y) - self.top) / self.scale,
        )
    }

    /// A design-space length in window pixels.
    pub(super) fn length(self, design: f64) -> Pixels {
        pixels(design * self.scale)
    }
}

/// One tile's resident frame, ready to paint.
pub(super) struct Scene<'frame> {
    /// The frame's shapes.
    pub(super) shapes: &'frame [Shape],
    /// The effect, for the mark's accent beam.
    pub(super) effect: Effect,
    /// Layer opacity.
    pub(super) alpha: f64,
    /// Ink colour.
    pub(super) ink: Rgb,
    /// Accent colour.
    pub(super) accent: Rgb,
    /// Where the design box sits.
    pub(super) placement: Placement,
}

impl Scene<'_> {
    /// Paint every shape and return how many could not be tessellated.
    pub(super) fn paint(&self, window: &mut Window) -> u64 {
        let ink = colour(self.ink, self.alpha);
        let accent = colour(self.accent, self.alpha);
        let mut failures = 0_u64;
        for (index, shape) in self.shapes.iter().enumerate() {
            // The mark's twenty-fourth beam is the brand's one accent.
            let fill = if self.effect == Effect::Mark && index == 23 {
                accent
            } else {
                ink
            };
            if !self.paint_shape(window, *shape, fill) {
                failures = failures.saturating_add(1);
            }
        }
        failures
    }

    /// Paint one shape; `false` when its path could not be built.
    fn paint_shape(&self, window: &mut Window, shape: Shape, fill: Hsla) -> bool {
        let place = self.placement;
        match shape {
            Shape::Circle {
                centre_x,
                centre_y,
                radius,
            } => {
                if radius > 0.0 {
                    let origin = place.point(centre_x - radius, centre_y - radius);
                    let side = place.length(radius * 2.0);
                    window.paint_quad(
                        gpui::fill(Bounds::new(origin, size(side, side)), fill)
                            .corner_radii(place.length(radius)),
                    );
                }
                true
            }
            Shape::Ellipse {
                centre_x,
                centre_y,
                radius_x,
                radius_y,
            } => {
                let radii = point(place.length(radius_x), place.length(radius_y));
                let mut builder = PathBuilder::fill();
                builder.move_to(place.point(centre_x + radius_x, centre_y));
                builder.arc_to(
                    radii,
                    pixels(0.0),
                    false,
                    false,
                    place.point(centre_x - radius_x, centre_y),
                );
                builder.arc_to(
                    radii,
                    pixels(0.0),
                    false,
                    false,
                    place.point(centre_x + radius_x, centre_y),
                );
                builder.close();
                builder
                    .build()
                    .map(|path| window.paint_path(path, fill))
                    .is_ok()
            }
            Shape::Quad { points } => {
                let corners = points.map(|corner| place.point(corner[0], corner[1]));
                let mut builder = PathBuilder::fill();
                builder.add_polygon(&corners, true);
                builder
                    .build()
                    .map(|path| window.paint_path(path, fill))
                    .is_ok()
            }
            Shape::Rect {
                left,
                top,
                right,
                bottom,
            } => {
                let origin = place.point(left, top);
                let extent = size(place.length(right - left), place.length(bottom - top));
                window.paint_quad(gpui::fill(Bounds::new(origin, extent), fill));
                true
            }
        }
    }
}

/// The coordinate box an effect draws in.
///
/// The mark is fixed brand geometry in a 100-unit box and the loader orbits
/// a 60-unit box; every other effect fills its canvas.
pub(super) const fn design_box(effect: Effect) -> (f64, f64) {
    match effect {
        Effect::Mark => (100.0, 100.0),
        Effect::Loader => (60.0, 60.0),
        _ => (TILE_WIDTH, TILE_HEIGHT),
    }
}

/// A brand colour at an opacity, as GPUI's colour type.
pub(super) fn colour(rgb: Rgb, alpha: f64) -> Hsla {
    Hsla::from(gpui::rgb(u32::from_be_bytes([
        0, rgb.red, rgb.green, rgb.blue,
    ])))
    .opacity(narrow(alpha.clamp(0.0, 1.0)))
}

#[cfg(test)]
mod tests {
    use super::{Placement, design_box};
    use crate::ink_lab::Effect;
    use crate::runtime::Percentiles;
    use lgwks_deps::gpui::{Bounds, point, px, size};

    #[test]
    fn percentiles_use_nearest_rank_and_report_empty_sets() {
        let empty = Percentiles::of(&[]);
        assert_eq!(empty.samples, 0, "empty set must say so");
        let samples: Vec<f64> = (1..=100).map(f64::from).collect();
        let stats = Percentiles::of(&samples);
        assert_eq!(stats.samples, 100, "sample count");
        assert!((stats.p50 - 50.0).abs() < 1e-9, "p50 {}", stats.p50);
        assert!((stats.p99 - 99.0).abs() < 1e-9, "p99 {}", stats.p99);
        assert!((stats.max - 100.0).abs() < 1e-9, "max {}", stats.max);
    }

    #[test]
    fn placement_round_trips_pointer_coordinates() {
        let bounds = Bounds::new(point(px(40.0), px(30.0)), size(px(248.0), px(168.0)));
        let placement = Placement::new(bounds, design_box(Effect::Mark));
        let (design_x, design_y) = placement.to_design(placement.point(59.34, 51.0));
        assert!((design_x - 59.34).abs() < 1e-3, "x {design_x}");
        assert!((design_y - 51.0).abs() < 1e-3, "y {design_y}");
    }
}
