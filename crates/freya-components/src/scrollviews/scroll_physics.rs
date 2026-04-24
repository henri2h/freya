use std::time::{
    Duration,
    Instant,
};

use freya_core::prelude::*;
use torin::geometry::CursorPoint;

use crate::scrollviews::ScrollController;

const VELOCITY_WINDOW_MS: u64 = 100;

/// Minimum fling velocity in pixels/second to trigger momentum scrolling.
pub const MIN_FLING_VELOCITY: f32 = 50.0;

/// Friction coefficient (1/s). Higher values decelerate faster.
const FRICTION: f32 = 1.0;

/// Velocity threshold in pixels/second below which momentum scrolling stops.
const STOP_VELOCITY: f32 = 10.0;

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
    pub fn velocity(&self) -> (f32, f32) {
        if self.samples.len() < 2 {
            return (0.0, 0.0);
        }
        let (p0, t0) = &self.samples[0];
        let (p1, t1) = &self.samples[self.samples.len() - 1];
        let dt = t1.duration_since(*t0).as_secs_f32();
        if dt < 0.001 {
            return (0.0, 0.0);
        }
        (
            (p1.x - p0.x) as f32 / dt,
            (p1.y - p0.y) as f32 / dt,
        )
    }
}

/// Frame-by-frame momentum animation driven after a fling gesture.
///
/// `vel_x` / `vel_y` are the finger velocity in pixels/second at release
/// (positive = moving right/down). Scroll position is updated each frame using
/// exponential friction decay and clamped to the scroll bounds.
pub async fn momentum_scroll(
    mut scroll_controller: ScrollController,
    vel_x: f32,
    vel_y: f32,
    inner_width: f32,
    inner_height: f32,
    viewport_width: f32,
    viewport_height: f32,
) {
    let mut ticker = RenderingTicker::get();
    let mut vx = vel_x;
    let mut vy = vel_y;

    let (raw_x, raw_y): (i32, i32) = scroll_controller.into();
    let mut pos_x = raw_x as f32;
    let mut pos_y = raw_y as f32;
    let mut prev = Instant::now();

    loop {
        ticker.tick().await;
        let now = Instant::now();
        let dt = now.duration_since(prev).as_secs_f32().min(0.05);
        prev = now;

        let decay = (-FRICTION * dt).exp();
        vx *= decay;
        vy *= decay;

        if vx.abs() < STOP_VELOCITY && vy.abs() < STOP_VELOCITY {
            break;
        }

        pos_x += vx * dt;
        pos_y += vy * dt;

        let max_x = -(inner_width - viewport_width).max(0.0);
        let max_y = -(inner_height - viewport_height).max(0.0);
        let clamped_x = pos_x.clamp(max_x, 0.0);
        let clamped_y = pos_y.clamp(max_y, 0.0);

        if clamped_x != pos_x {
            vx = 0.0;
            pos_x = clamped_x;
        }
        if clamped_y != pos_y {
            vy = 0.0;
            pos_y = clamped_y;
        }

        scroll_controller.scroll_to_x(pos_x as i32);
        scroll_controller.scroll_to_y(pos_y as i32);
    }
}
