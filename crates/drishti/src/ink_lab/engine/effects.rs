//! The per-effect advances: each moves one host's state by a frame delta and
//! stamps that effect's shapes into the frame.

use super::beams::{beam_geometry, brand_default, index_to_float};
use super::{Bit, Drop, Host, Spot, clamp01, in_out, settle};
use crate::ink_lab::{Frame, MAX_BEADS, Shape};

impl Host {
    /// Mark: twenty-four beams squirt outward, fuse, then go crisp.
    pub(super) fn advance_mark(&mut self, dt_ms: f64, frame: &mut Frame) {
        let options = self.options;
        let duration = brand_default(options.duration_ms, 2200.0);
        let hold = brand_default(options.hold_ms, 1500.0);
        let stagger = brand_default(options.stagger_ms, 55.0);
        let loop_forever = brand_default(options.loop_forever, true);
        let total = duration + hold + 400.0;
        self.state.mark_time += dt_ms;
        let mut time = self.state.mark_time;
        if time > total {
            if loop_forever {
                self.state.mark_time = 0.0;
                time = 0.0;
            } else {
                self.state.mark_time = total;
                time = total;
                self.finished = true;
            }
        }
        let crisp = clamp01((time - 1350.0) / 500.0);
        frame.alpha = if !loop_forever && time > duration + hold {
            clamp01(1.0 - (time - duration - hold) / 350.0)
        } else {
            1.0
        };
        let beams = beam_geometry();
        if crisp >= 1.0 {
            for beam in beams {
                Host::push(frame, beam.quad());
            }
            return;
        }
        for (index, beam) in beams.iter().enumerate() {
            let index_float = index_to_float(index);
            let group_progress = clamp01((time - index_float * stagger) / 900.0);
            let count = beam.blob_count();
            let mut blob = 0_usize;
            while index_to_float(blob) < count {
                let fraction = if count <= 1.0 {
                    0.0
                } else {
                    index_to_float(blob) / (count - 1.0)
                };
                let ease = settle(clamp01(group_progress * 1.35 - fraction * 0.35));
                let travel = fraction * ease;
                let width = beam.half_start + (beam.half_end - beam.half_start) * fraction;
                let wobble = (time * 0.006 + index_float * 1.7 + fraction * 0.9).sin()
                    * (1.0 - group_progress)
                    * 1.2
                    * ease;
                let radius = ease * (width * 0.95 + 0.35);
                let centre_x =
                    beam.start[0] + (beam.end[0] - beam.start[0]) * travel + beam.normal_x * wobble;
                let centre_y =
                    beam.start[1] + (beam.end[1] - beam.start[1]) * travel + beam.normal_y * wobble;
                Host::push(
                    frame,
                    Shape::Circle {
                        centre_x,
                        centre_y,
                        radius,
                    },
                );
                blob = blob.wrapping_add(1);
            }
        }
    }

    /// Text: letters condense out of a blurred, turbulent pool.
    pub(super) fn advance_text(&mut self, dt_ms: f64, frame: &mut Frame) {
        let duration = brand_default(self.options.duration_ms, 1500.0);
        let hold = brand_default(self.options.hold_ms, 1800.0);
        let loop_forever = brand_default(self.options.loop_forever, true);
        let blur_max = brand_default(self.options.blur, 48.0 * 0.085);
        let displacement_max = brand_default(self.options.displacement, 48.0 * 0.5);
        self.state.elapsed += dt_ms;
        let total = duration + hold + 350.0;
        if self.state.elapsed > total {
            if loop_forever {
                self.state.elapsed = 0.0;
            } else {
                self.state.elapsed = total;
                self.finished = true;
            }
        }
        let progress = settle(clamp01(self.state.elapsed / duration));
        if progress >= 0.999 {
            frame.blur = 0.0;
            frame.displacement = 0.0;
            frame.alpha = 1.0;
            return;
        }
        frame.blur = (1.0 - progress) * blur_max;
        frame.displacement = (1.0 - progress) * displacement_max;
        frame.alpha = clamp01(self.state.elapsed / 150.0);
    }

    /// Wipe and reveal: a liquid level rises, holds and drains.
    pub(super) fn advance_wipe(&mut self, dt_ms: f64, frame: &mut Frame, reveal: bool) {
        let rise = brand_default(self.options.duration_ms, 1100.0);
        let hold = brand_default(self.options.hold_ms, 700.0);
        let drain = brand_default(self.options.duration_ms, 1000.0);
        self.state.phase_time += dt_ms;
        match self.state.phase {
            0 => {
                self.state.phase = if reveal { 3 } else { 1 };
                self.state.phase_time = 0.0;
            }
            1 => {
                self.state.level = in_out(clamp01(self.state.phase_time / rise));
                if self.state.phase_time >= rise {
                    self.state.phase = 2;
                    self.state.phase_time = 0.0;
                }
            }
            2 => {
                self.state.level = 1.0;
                if self.state.phase_time >= hold {
                    self.state.phase = 3;
                    self.state.phase_time = 0.0;
                }
            }
            3 => {
                self.state.level = 1.0 - in_out(clamp01(self.state.phase_time / drain));
                if self.state.phase_time >= drain {
                    self.state.phase = 0;
                    self.state.phase_time = 0.0;
                    self.finished = true;
                }
            }
            _ => {
                self.state.phase = 3;
                self.state.phase_time = 0.0;
            }
        }
        self.state.elapsed += dt_ms;
        let height = self.canvas.height;
        let width = self.canvas.width;
        let top = height * (1.0 - self.state.level) - self.state.level * 30.0;
        if self.state.level > 0.001 {
            Host::push(
                frame,
                Shape::Rect {
                    left: 0.0,
                    top: top + 20.0,
                    right: width,
                    bottom: height,
                },
            );
        }
        let columns = self.state.columns.len();
        if columns > 0 {
            let step = width / index_to_float(columns);
            for (index, (phase, size)) in self
                .state
                .columns
                .iter()
                .zip(self.state.column_sizes.iter().copied())
                .enumerate()
            {
                let amplitude = (std::f64::consts::PI * clamp01(self.state.level)).sin()
                    + if self.state.level > 0.0 && self.state.level < 1.0 {
                        0.25
                    } else {
                        0.0
                    };
                let radius = size
                    * (0.55 + 0.45 * (self.state.elapsed * 0.003 + *phase).sin())
                    * (0.4 + amplitude);
                let centre_x = index_to_float(index) * step + step * 0.5;
                let centre_y = top + 12.0 + (self.state.elapsed * 0.004 + phase * 2.0).sin() * 10.0;
                Host::push(
                    frame,
                    Shape::Circle {
                        centre_x,
                        centre_y,
                        radius: radius.max(0.0),
                    },
                );
            }
        }
    }

    /// Drops: a drop swells, falls, splashes into a living puddle.
    pub(super) fn advance_drops(&mut self, dt_ms: f64, frame: &mut Frame) {
        let size = brand_default(self.options.size, 15.0);
        let spout = brand_default(self.options.spout, 0.5);
        let width = self.canvas.width;
        let height = self.canvas.height;
        self.state.elapsed += dt_ms;
        let spout_x = width * spout;
        self.state.hang += dt_ms * 0.0038 * (1.0 + self.state.hang / 10.0);
        let hang_y = self.state.hang * 1.1 + 6.0;
        if self.state.hang >= size {
            if self.state.drops.len() < 64 {
                self.state.drops.push(Drop {
                    centre_x: spout_x,
                    centre_y: hang_y,
                    velocity_y: 0.05,
                    radius: self.state.hang * 0.8,
                    landed: false,
                });
            }
            self.state.hang = 2.5;
        }
        Host::push(
            frame,
            Shape::Circle {
                centre_x: spout_x,
                centre_y: 0.0,
                radius: self.state.hang * 0.7 + 3.0,
            },
        );
        for step in 0..=6 {
            let fraction = index_to_float(step) / 6.0;
            Host::push(
                frame,
                Shape::Circle {
                    centre_x: spout_x,
                    centre_y: hang_y * fraction,
                    radius: (self.state.hang * (0.42 + 0.1 * fraction)).max(2.2),
                },
            );
        }
        Host::push(
            frame,
            Shape::Circle {
                centre_x: spout_x,
                centre_y: hang_y,
                radius: self.state.hang,
            },
        );
        let floor = height - 2.0;
        for drop in &mut self.state.drops {
            if drop.landed {
                continue;
            }
            drop.velocity_y += 0.0009 * dt_ms;
            drop.centre_y += drop.velocity_y * dt_ms;
            Host::push(
                frame,
                Shape::Ellipse {
                    centre_x: drop.centre_x,
                    centre_y: drop.centre_y,
                    radius_x: drop.radius * 0.85,
                    radius_y: drop.radius * (1.0 + (0.5_f64).min(drop.velocity_y * 0.5)),
                },
            );
            if drop.centre_y + drop.radius >= floor - self.state.puddle {
                self.state.puddle += drop.radius * 0.32;
                self.state.puddle_amplitude += drop.radius * 0.7;
                for _ in 0..3 {
                    if self.state.bits.len() < 128 {
                        self.state.bits.push(Bit {
                            centre_x: drop.centre_x,
                            centre_y: floor - self.state.puddle,
                            velocity_x: self.rng.range(-0.125, 0.125),
                            velocity_y: -self.rng.range(0.15, 0.35),
                            radius: drop.radius * self.rng.range(0.15, 0.27),
                        });
                    }
                }
                drop.landed = true;
            }
        }
        self.state.drops.retain(|drop| !drop.landed);
        for bit in &mut self.state.bits {
            bit.velocity_y += 0.0009 * dt_ms;
            bit.centre_x += bit.velocity_x * dt_ms;
            bit.centre_y += bit.velocity_y * dt_ms;
            Host::push(
                frame,
                Shape::Circle {
                    centre_x: bit.centre_x,
                    centre_y: bit.centre_y,
                    radius: bit.radius,
                },
            );
        }
        self.state
            .bits
            .retain(|bit| bit.centre_y <= floor - self.state.puddle + 2.0);
        self.state.puddle += (5.0 - self.state.puddle) * 0.0009 * dt_ms;
        self.state.puddle_amplitude *= 0.997_f64.powf(dt_ms);
        let mut position_x = 0.0;
        while position_x <= width {
            let radius = self.state.puddle * 0.9
                + (position_x * 0.045 - self.state.elapsed * 0.008).sin()
                    * self.state.puddle_amplitude
                    * 0.18
                + (position_x * 0.11 + self.state.elapsed * 0.005).sin()
                    * self.state.puddle_amplitude
                    * 0.1;
            Host::push(
                frame,
                Shape::Circle {
                    centre_x: position_x,
                    centre_y: floor - radius * 0.2 + 4.0,
                    radius: radius.max(2.0),
                },
            );
            position_x += 8.0;
        }
    }

    /// Wobble: an idle liquid edge leans toward the pointer.
    pub(super) fn advance_wobble(&mut self, dt_ms: f64, frame: &mut Frame) {
        let amount = brand_default(self.options.amount, 2.2);
        let hover = brand_default(self.options.hover, 7.0);
        let target = if self.input.pointer.is_some() {
            hover
        } else {
            amount
        };
        self.state.elapsed += dt_ms;
        self.state.wobble_current += (target - self.state.wobble_current) * 0.12;
        frame.displacement = self.state.wobble_current.max(amount * 0.5);
    }

    /// Loader: three drops orbit, fuse and part.
    pub(super) fn advance_loader(&mut self, dt_ms: f64, frame: &mut Frame) {
        self.state.elapsed += dt_ms;
        let time = self.state.elapsed;
        let centre = 30.0;
        Host::push(
            frame,
            Shape::Circle {
                centre_x: centre,
                centre_y: centre,
                radius: 8.0 + 1.6 * (time * 0.006).sin(),
            },
        );
        for index in 0..3 {
            let index_float = index_to_float(index);
            let angle = time * 0.0042 + index_float * 2.094;
            let radius = 15.0 + 7.0 * (time * 0.0026 + index_float * 2.1).sin();
            Host::push(
                frame,
                Shape::Circle {
                    centre_x: centre + angle.cos() * radius,
                    centre_y: centre + angle.sin() * radius,
                    radius: 6.5,
                },
            );
        }
    }

    /// Pour: a progress bar of liquid that leans at the meniscus.
    pub(super) fn advance_pour(&mut self, dt_ms: f64, frame: &mut Frame) {
        if self.state.pour_target == 0.0 {
            self.state.pour_target = brand_default(self.options.level, 0.6);
        }
        self.state.elapsed += dt_ms;
        self.state.pour_level +=
            (self.state.pour_target - self.state.pour_level) * (dt_ms * 0.004).min(1.0);
        let height = self.canvas.height;
        let width = self.canvas.width;
        let radius = height * 0.5;
        let front = self.state.pour_level * (width - height * 0.5);
        if self.state.pour_level > 0.002 {
            Host::push(
                frame,
                Shape::Rect {
                    left: 0.0,
                    top: 0.0,
                    right: front + radius * 0.4,
                    bottom: height,
                },
            );
            for index in 0..4 {
                let index_float = index_to_float(index);
                Host::push(
                    frame,
                    Shape::Circle {
                        centre_x: front
                            + radius
                                * (0.55
                                    + 0.35
                                        * (self.state.elapsed * 0.006 + index_float * 1.7).sin()),
                        centre_y: height * (0.2 + 0.2 * index_float)
                            + (self.state.elapsed * 0.005 + index_float).sin() * height * 0.06,
                        radius: radius
                            * (0.62 + 0.2 * (self.state.elapsed * 0.004 + index_float * 2.0).sin()),
                    },
                );
            }
        }
        let velocity = (self.state.pour_target - self.state.pour_level).abs();
        if velocity > 0.01 {
            Host::push(
                frame,
                Shape::Circle {
                    centre_x: front + radius * 1.7 + velocity * 30.0,
                    centre_y: height * 0.5 + (self.state.elapsed * 0.01).sin() * 2.0,
                    radius: radius * 0.32,
                },
            );
        }
    }

    /// Cursor: a chain of beads follows the pointer and wanders when idle.
    pub(super) fn advance_cursor(&mut self, dt_ms: f64, frame: &mut Frame) {
        let size = brand_default(self.options.size, 30.0);
        let count = brand_default(self.options.beads, 8).min(MAX_BEADS);
        let width = self.canvas.width;
        let height = self.canvas.height;
        while self.state.beads.len() < count {
            self.state.beads.push((width * 0.5, height * 0.5));
        }
        self.state.beads.truncate(count);
        self.state.elapsed += dt_ms;
        self.state.wander += dt_ms;
        let inside = self.input.pointer.is_some();
        let target_alive = if inside { 1.0 } else { 0.0 };
        self.state.cursor_alive +=
            (target_alive - self.state.cursor_alive) * (dt_ms * 0.006).min(1.0);
        let (target_x, target_y) = match self.input.pointer {
            Some((pointer_x, pointer_y)) => (pointer_x, pointer_y),
            None => (
                width * 0.5 + (self.state.wander * 0.0011).cos() * width * 0.3,
                height * 0.5 + (self.state.wander * 0.0017).sin() * height * 0.28,
            ),
        };
        let mut previous = (target_x, target_y);
        let bead_count = self.state.beads.len();
        for (index, bead) in self.state.beads.iter_mut().enumerate() {
            let index_float = index_to_float(index);
            let rate = (dt_ms * (0.02 - index_float * 0.0011)).min(1.0);
            let pull = if index == 0 { rate * 1.6 } else { rate * 1.3 };
            bead.0 += (previous.0 - bead.0) * pull;
            bead.1 += (previous.1 - bead.1) * pull;
            let life = if index == 0 { 1.0 } else { 0.86 };
            let radius = (size
                * (1.0 - index_float / (index_to_float(bead_count) + 1.0))
                * (0.55 + 0.45 * self.state.cursor_alive)
                * life)
                .max(1.0);
            Host::push(
                frame,
                Shape::Circle {
                    centre_x: bead.0,
                    centre_y: bead.1,
                    radius,
                },
            );
            previous = (bead.0, bead.1);
        }
    }

    /// Ripple: a click splashes ink petals that spread and thin out.
    pub(super) fn advance_ripple(&mut self, dt_ms: f64, frame: &mut Frame) {
        let size = brand_default(self.options.size, 90.0);
        let duration = brand_default(self.options.duration_ms, 1100.0);
        let auto = brand_default(self.options.auto_ms, 0.0);
        let width = self.canvas.width;
        let height = self.canvas.height;
        if let Some((click_x, click_y)) = self.input.click
            && self.state.spots.len() < 24
        {
            let petals = 9.0 + (self.rng.unit() * 4.0).floor();
            self.state.spots.push(Spot {
                centre_x: click_x,
                centre_y: click_y,
                elapsed: 0.0,
                petals,
                phase: self.rng.range(0.0, std::f64::consts::TAU),
            });
        }
        if auto > 0.0 {
            self.state.auto_accumulator += dt_ms;
            if self.state.auto_accumulator >= auto {
                self.state.auto_accumulator = 0.0;
                if self.state.spots.len() < 24 {
                    self.state.spots.push(Spot {
                        centre_x: width * (0.2 + self.rng.unit() * 0.6),
                        centre_y: height * (0.25 + self.rng.unit() * 0.5),
                        elapsed: 0.0,
                        petals: 9.0 + (self.rng.unit() * 4.0).floor(),
                        phase: self.rng.range(0.0, std::f64::consts::TAU),
                    });
                }
            }
        }
        self.state.elapsed += dt_ms;
        for spot in &mut self.state.spots {
            spot.elapsed += dt_ms;
            let progress = clamp01(spot.elapsed / duration);
            if progress >= 1.0 {
                continue;
            }
            let eased = settle(progress);
            let ring = size * eased;
            let thickness = size * 0.28 * (1.0 - progress);
            Host::push(
                frame,
                Shape::Circle {
                    centre_x: spot.centre_x,
                    centre_y: spot.centre_y,
                    radius: size * 0.22 * (1.0 - progress) * (1.0 - progress),
                },
            );
            let mut petal = 0.0;
            while petal < spot.petals {
                let angle = petal / spot.petals * std::f64::consts::TAU + spot.phase;
                let radius = ring * (0.86 + 0.28 * (angle * 3.0 + spot.phase).sin());
                Host::push(
                    frame,
                    Shape::Circle {
                        centre_x: spot.centre_x + angle.cos() * radius,
                        centre_y: spot.centre_y + angle.sin() * radius,
                        radius: (thickness * (0.6 + 0.4 * (angle * 5.0 + spot.phase).sin()))
                            .max(0.5),
                    },
                );
                petal += 1.0;
            }
        }
        self.state.spots.retain(|spot| spot.elapsed < duration);
    }

    /// Count: a number counts up in tabular figures.
    pub(super) fn advance_count(&mut self, dt_ms: f64, frame: &mut Frame) {
        let duration = brand_default(self.options.duration_ms, 1600.0);
        let hold = brand_default(self.options.hold_ms, 1800.0);
        let loop_forever = brand_default(self.options.loop_forever, true);
        self.state.elapsed += dt_ms;
        let total = duration + hold;
        if self.state.elapsed > total {
            if loop_forever {
                self.state.elapsed = 0.0;
            } else {
                self.state.elapsed = total;
                self.finished = true;
            }
        }
        let progress = if self.state.elapsed >= duration {
            1.0
        } else {
            1.0 - (1.0 - clamp01(self.state.elapsed / duration)).powi(3)
        };
        frame.label = Some(self.count_label(progress));
    }
}
