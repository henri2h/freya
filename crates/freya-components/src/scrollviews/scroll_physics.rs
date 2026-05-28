use std::time::{
    Duration,
    Instant,
};

use freya_core::prelude::*;
use torin::geometry::CursorPoint;

use crate::scrollviews::ScrollController;

// Android OverScroller constants (see SplineOverScroller)
/// Shape of the deceleration curve: `ln(0.78) / ln(0.9)`.
const DECELERATION_RATE: f32 = 2.358;
const INFLEXION: f32 = 0.35;
const FLING_FRICTION: f32 = 0.015;
/// Physical coefficient tuned for ~96 DPI logical pixels.
/// On Android this is `GRAVITY_EARTH * 39.37 * ppi * 0.84`; at 96 DPI that is
/// ~31 000, but we use a lower value so that fling distances feel right for
/// the typical pointer-speed range on desktop/touch surfaces.
const PHYSICAL_COEFF: f32 = 23_000.0;

/// Minimum fling speed (px/s) that triggers a momentum animation.
pub const MIN_FLING_VELOCITY: f32 = 150.0;
/// Maximum fling speed (px/s), matching Android's `getScaledMaximumFlingVelocity`.
const MAX_FLING_VELOCITY: f32 = 8_000.0;

const VELOCITY_WINDOW_MS: u64 = 100;
/// How many of the most-recent samples to use when estimating release velocity.
/// Using only the last few samples avoids averaging in earlier, slower movement.
const VELOCITY_SAMPLES: usize = 3;

/// Rolling-window velocity tracker for drag gestures.
#[derive(Default, Clone)]
pub struct VelocityTracker {
    samples: Vec<(CursorPoint, Instant)>,
}

impl VelocityTracker {
    pub fn push(&mut self, position: CursorPoint) {
        let now = Instant::now();
        self.samples.push((position, now));
        if let Some(cutoff) = now.checked_sub(Duration::from_millis(VELOCITY_WINDOW_MS)) {
            self.samples.retain(|(_, t)| *t >= cutoff);
        }
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }

    /// Returns velocity in pixels/second as `(vx, vy)`.
    ///
    /// Only the most recent [`VELOCITY_SAMPLES`] are used so that the estimate
    /// reflects the instantaneous speed at release rather than the average over
    /// the whole gesture.
    pub fn velocity(&self) -> (f32, f32) {
        let len = self.samples.len();
        if len < 2 {
            return (0.0, 0.0);
        }
        let start = len.saturating_sub(VELOCITY_SAMPLES);
        let (p0, t0) = &self.samples[start];
        let (p1, t1) = &self.samples[len - 1];
        let dt = t1.checked_duration_since(*t0).map(|d| d.as_secs_f32()).unwrap_or(0.0);
        if dt < 0.001 {
            return (0.0, 0.0);
        }
        let vx = ((p1.x - p0.x) as f32 / dt).clamp(-MAX_FLING_VELOCITY, MAX_FLING_VELOCITY);
        let vy = ((p1.y - p0.y) as f32 / dt).clamp(-MAX_FLING_VELOCITY, MAX_FLING_VELOCITY);
        (vx, vy)
    }
}

/// Pre-compute total fling distance (px) from a speed (px/s) and pre-computed duration (s).
///
/// Derived from `v(t) = speed * (1 - t/T)^(DECELERATION_RATE - 1)`, consistent
/// with Flutter's `ClampingScrollSimulation`: `D = speed * T / DECELERATION_RATE`.
fn fling_distance(speed: f32, duration: f32) -> f32 {
    speed * duration / DECELERATION_RATE
}

/// Pre-compute total fling duration (seconds) from a given speed (px/s).
///
/// Matches Android's `SplineOverScroller.getSplineFlingDuration`.
fn fling_duration_secs(speed: f32) -> f32 {
    let a = FLING_FRICTION * PHYSICAL_COEFF;
    let ratio = INFLEXION * speed / a;
    if ratio <= 0.0 {
        return 0.0;
    }
    let l = ratio.ln();
    (l / (DECELERATION_RATE - 1.0)).exp()
}

/// Momentum animation driven after a fling gesture.
///
/// Uses Android-inspired spline deceleration: total fling distance `D` and
/// duration `T` are pre-computed from the initial velocity, then each frame
/// advances position along `1 − (1 − t)^DECELERATION_RATE` (fast start,
/// smooth tail). Scroll is clamped at the content boundaries (no over-scroll).
///
/// `vel_x` / `vel_y`: finger velocity in px/s at gesture release.
/// A positive value means the finger was moving right/down, which scrolls the
/// content in that direction (natural/iOS-style scrolling).
pub async fn momentum_scroll(
    mut scroll_controller: ScrollController,
    vel_x: f32,
    vel_y: f32,
    inner_width: f32,
    inner_height: f32,
    viewport_width: f32,
    viewport_height: f32,
) {
    let speed = (vel_x * vel_x + vel_y * vel_y).sqrt();
    if speed < MIN_FLING_VELOCITY {
        return;
    }

    let total_duration = fling_duration_secs(speed);
    let total_distance = fling_distance(speed, total_duration);
    if total_duration <= 0.0 || total_distance <= 0.0 {
        return;
    }

    let (raw_x, raw_y): (i32, i32) = scroll_controller.into();
    let start_x = raw_x as f32;
    let start_y = raw_y as f32;

    // Split total distance along each axis proportionally to its velocity share
    let dist_x = total_distance * (vel_x / speed);
    let dist_y = total_distance * (vel_y / speed);

    // Clamp target to scroll bounds (ClampingScrollPhysics — no bounce)
    let max_x = -(inner_width - viewport_width).max(0.0);
    let max_y = -(inner_height - viewport_height).max(0.0);
    let actual_dist_x = (start_x + dist_x).clamp(max_x, 0.0) - start_x;
    let actual_dist_y = (start_y + dist_y).clamp(max_y, 0.0) - start_y;

    if actual_dist_x.abs() < 1.0 && actual_dist_y.abs() < 1.0 {
        return;
    }

    let mut ticker = RenderingTicker::get();
    let start_time = Instant::now();

    loop {
        ticker.tick().await;
        let t = (start_time.elapsed().as_secs_f32() / total_duration).min(1.0);
        // Android spline curve approximation: fast at start, decelerates smoothly
        let curve = 1.0 - (1.0 - t).powf(DECELERATION_RATE);

        scroll_controller.scroll_to_x((start_x + actual_dist_x * curve) as i32);
        scroll_controller.scroll_to_y((start_y + actual_dist_y * curve) as i32);

        if t >= 1.0 {
            break;
        }
    }
}
