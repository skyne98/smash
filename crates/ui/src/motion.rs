//! Motion: every visible state change is followed by a short spring
//! animation, the same in the terminal and the window.
//!
//! State stays discrete and instant (logic and tests never wait for an
//! animation). The view derives each animated value from its target once per
//! frame with [`Motions::follow`], so a change of state *is* the start of its
//! animation, and a change mid-flight retargets the spring with its velocity
//! kept: interruptible, never a restart from zero.
//!
//! Springs are solved in closed form (mass 1), so any frame rate gives the same
//! motion: one 100 ms step lands exactly where ten 10 ms steps do. Tuned by
//! feel the way UIKit/SwiftUI are: `response` (period of the undamped spring,
//! seconds) and `damping` ratio (1 = fastest without overshoot, below 1
//! overshoots a little).
//!
//! `SMASH_REDUCED_MOTION=1` (or `NO_MOTION`) snaps every value: motion is
//! never required to read the screen.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::layout::Run;

/// A spring, by feel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    /// Period of the undamped oscillation, seconds: smaller is snappier.
    pub response: f64,
    /// 1 = critically damped (no overshoot); 0.7 overshoots by about 5 %.
    pub damping: f64,
}

impl Spring {
    /// Scrolling and anything that moves content: fast, no overshoot.
    pub const GLIDE: Spring = Spring {
        response: 0.16,
        damping: 1.0,
    };
    /// Things appearing (rows, notices, highlights): fade in quickly.
    pub const FADE: Spring = Spring {
        response: 0.14,
        damping: 1.0,
    };
    /// Popups opening: a small overshoot that reads as "it popped".
    pub const POP: Spring = Spring {
        response: 0.2,
        damping: 0.72,
    };
    /// Numbers counting to a new value (percentages, totals).
    pub const COUNT: Spring = Spring {
        response: 0.3,
        damping: 1.0,
    };

    fn omega(self) -> f64 {
        std::f64::consts::TAU / self.response.max(1e-3)
    }

    /// Position and velocity after `dt` seconds, from offset `y` (value minus
    /// target) moving at `v`. Exact for every damping ratio.
    pub fn solve(self, y: f64, v: f64, dt: f64) -> (f64, f64) {
        let w = self.omega();
        let z = self.damping.max(0.0);
        if (z - 1.0).abs() < 1e-6 {
            let e = (-w * dt).exp();
            let c = v + w * y;
            return ((y + c * dt) * e, (v - w * c * dt) * e);
        }
        if z < 1.0 {
            let wd = w * (1.0 - z * z).sqrt();
            let e = (-z * w * dt).exp();
            let (sin, cos) = (wd * dt).sin_cos();
            let a = (v + z * w * y) / wd;
            let b = (z * w * v + w * w * y) / wd;
            return (e * (y * cos + a * sin), e * (v * cos - b * sin));
        }
        let root = (z * z - 1.0).sqrt();
        let (r1, r2) = (-w * (z - root), -w * (z + root));
        let c2 = (v - r1 * y) / (r2 - r1);
        let c1 = y - c2;
        let (e1, e2) = ((r1 * dt).exp(), (r2 * dt).exp());
        (c1 * e1 + c2 * e2, r1 * c1 * e1 + r2 * c2 * e2)
    }
}

/// Offset (in the value's units) and speed (units/s) below which a spring is
/// at rest: a 200th of a row or of full opacity is invisible.
const REST: f64 = 0.005;
const REST_SPEED: f64 = 0.05;
/// Longest step one frame may take: a frame after a stall continues the
/// motion instead of jumping to its end.
const MAX_STEP: f64 = 1.0 / 30.0;

/// One animated value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Motion {
    pub value: f64,
    pub velocity: f64,
    pub target: f64,
    pub spring: Spring,
}

impl Motion {
    pub fn at(value: f64, spring: Spring) -> Self {
        Self {
            value,
            velocity: 0.0,
            target: value,
            spring,
        }
    }

    pub fn step(&mut self, dt: f64) {
        if self.resting() {
            self.value = self.target;
            self.velocity = 0.0;
            return;
        }
        let (y, v) = self
            .spring
            .solve(self.value - self.target, self.velocity, dt);
        self.value = self.target + y;
        self.velocity = v;
        if self.resting() {
            self.value = self.target;
            self.velocity = 0.0;
        }
    }

    pub fn resting(&self) -> bool {
        (self.value - self.target).abs() < REST && self.velocity.abs() < REST_SPEED
    }
}

/// What a motion animates; one value per key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// Transcript scroll, rows from the bottom.
    Scroll,
    /// Opacity of the whole transcript (session switch, relayout).
    Content,
    /// Opacity of one transcript row, by absolute index.
    Row(usize),
    /// Popup openness: 0 closed, 1 open.
    Popup,
    /// Opacity of the highlighted popup item.
    Pick,
    /// Opacity of the status notice.
    Notice,
    /// Prompt box height, rows.
    Input,
    /// Context meter, share of the window.
    Context,
    /// Subagent dock height, rows.
    Dock,
    /// An open list's scroll position, rows.
    List,
}

/// Every running motion. Protocol, once per frame: [`Motions::tick`] first,
/// then `follow`/`start`/`nudge` while the frame is built. A change made
/// that way starts at the frame it is first drawn in, however long the screen
/// sat idle before it.
pub struct Motions {
    motions: HashMap<Key, Motion>,
    last: Option<Instant>,
    /// Snap instead of animating.
    pub reduced: bool,
}

impl Default for Motions {
    fn default() -> Self {
        Self::new(reduced_from_env())
    }
}

/// `SMASH_REDUCED_MOTION` set to anything but `0`, or `NO_MOTION` set.
pub fn reduced_from_env() -> bool {
    let set = |name: &str| std::env::var(name).is_ok_and(|value| value != "0" && !value.is_empty());
    set("SMASH_REDUCED_MOTION") || set("NO_MOTION")
}

impl Motions {
    pub fn new(reduced: bool) -> Self {
        Self {
            motions: HashMap::new(),
            last: None,
            reduced,
        }
    }

    /// Advance everything to `now`. The first frame after a rest starts from
    /// zero elapsed time, so an animation never begins half finished.
    pub fn tick(&mut self, now: Instant) {
        let dt = match self.last {
            Some(last) if self.active() => now.saturating_duration_since(last).as_secs_f64(),
            _ => 0.0,
        };
        self.last = Some(now);
        let dt = dt.min(MAX_STEP);
        for motion in self.motions.values_mut() {
            motion.step(dt);
        }
        // Rows and other one-shots are dropped once they rest at their end.
        self.motions
            .retain(|key, motion| !(matches!(key, Key::Row(_)) && motion.resting()));
    }

    /// The animated value for `key` heading to `target`; the first call
    /// starts at the target (nothing to animate yet).
    pub fn follow(&mut self, key: Key, target: f64, spring: Spring) -> f64 {
        let reduced = self.reduced;
        let motion = self
            .motions
            .entry(key)
            .or_insert_with(|| Motion::at(target, spring));
        motion.target = target;
        motion.spring = spring;
        if reduced {
            *motion = Motion::at(target, spring);
        }
        motion.value
    }

    /// Start `key` at `from` heading to `to` (an entrance).
    pub fn start(&mut self, key: Key, from: f64, to: f64, spring: Spring) {
        let value = if self.reduced { to } else { from };
        self.motions.insert(
            key,
            Motion {
                value,
                velocity: 0.0,
                target: to,
                spring,
            },
        );
    }

    /// Shift `key`'s current value by `by`, target unchanged: content that
    /// jumped stays put on screen, then glides to where it now belongs.
    pub fn nudge(&mut self, key: Key, by: f64) {
        if self.reduced {
            return;
        }
        if let Some(motion) = self.motions.get_mut(&key) {
            motion.value += by;
        }
    }

    /// Jump `key` to `value`, at rest.
    pub fn snap(&mut self, key: Key, value: f64) {
        if let Some(motion) = self.motions.get_mut(&key) {
            *motion = Motion::at(value, motion.spring);
        }
    }

    pub fn value(&self, key: &Key) -> Option<f64> {
        self.motions.get(key).map(|motion| motion.value)
    }

    /// Clear all per-row motions (a different transcript is shown).
    pub fn forget_rows(&mut self) {
        self.motions.retain(|key, _| !matches!(key, Key::Row(_)));
    }

    /// Something is still moving: the client should draw at full frame rate.
    pub fn active(&self) -> bool {
        self.motions.values().any(|motion| !motion.resting())
    }
}

/// Frame interval while something moves, and while idle-but-alive.
pub const FAST: Duration = Duration::from_millis(16);
pub const SLOW: Duration = Duration::from_millis(50);

// ------------------------------------------------------------------ colour

/// `a` to `b` by `t` (clamped to 0..1), per sRGB channel.
pub fn mix(a: u32, b: u32, t: f64) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let channel = |shift: u32| {
        let (x, y) = (((a >> shift) & 0xff) as f64, ((b >> shift) & 0xff) as f64);
        ((x + (y - x) * t).round() as u32) << shift
    };
    channel(16) | channel(8) | channel(0)
}

/// `row` faded toward `bg` (t = 0 invisible, 1 as painted).
pub fn fade(row: &mut [Run], bg: u32, t: f64) {
    if t >= 1.0 {
        return;
    }
    for run in row.iter_mut() {
        run.paint.fg = mix(bg, run.paint.fg, t);
        if let Some(own) = run.paint.bg {
            run.paint.bg = Some(mix(bg, own, t));
        }
    }
}

/// `row`'s backgrounds from `from` to what they are, by `t` (a highlight
/// arriving); runs without a background are left alone.
pub fn tint(row: &mut [Run], from: u32, t: f64) {
    if t >= 1.0 {
        return;
    }
    for run in row.iter_mut() {
        if let Some(own) = run.paint.bg {
            run.paint.bg = Some(mix(from, own, t));
        }
    }
}

/// How many of `len` rows an opening at `t` shows (at least one while open).
pub fn reveal(len: usize, t: f64) -> usize {
    if t <= 0.0 {
        return 0;
    }
    ((len as f64 * t.min(1.0)).ceil() as usize)
        .clamp(1, len.max(1))
        .min(len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Paint, run};

    const SPRINGS: [Spring; 6] = [
        Spring::GLIDE,
        Spring::FADE,
        Spring::POP,
        Spring::COUNT,
        Spring {
            response: 0.2,
            damping: 1.6,
        },
        Spring {
            response: 0.3,
            damping: 0.3,
        },
    ];

    /// Reference: RK4 on x'' = -w²y - 2zw·v with a tiny step.
    fn numeric(spring: Spring, mut y: f64, mut v: f64, dt: f64) -> (f64, f64) {
        let w = spring.omega();
        let z = spring.damping;
        let accel = |y: f64, v: f64| -w * w * y - 2.0 * z * w * v;
        let steps = 20_000;
        let h = dt / steps as f64;
        for _ in 0..steps {
            let (k1y, k1v) = (v, accel(y, v));
            let (k2y, k2v) = (
                v + h / 2.0 * k1v,
                accel(y + h / 2.0 * k1y, v + h / 2.0 * k1v),
            );
            let (k3y, k3v) = (
                v + h / 2.0 * k2v,
                accel(y + h / 2.0 * k2y, v + h / 2.0 * k2v),
            );
            let (k4y, k4v) = (v + h * k3v, accel(y + h * k3y, v + h * k3v));
            y += h / 6.0 * (k1y + 2.0 * k2y + 2.0 * k3y + k4y);
            v += h / 6.0 * (k1v + 2.0 * k2v + 2.0 * k3v + k4v);
        }
        (y, v)
    }

    #[test]
    fn closed_form_matches_numeric_integration_for_every_damping() {
        for spring in SPRINGS {
            for (y, v) in [(1.0, 0.0), (-3.0, 4.0), (0.0, 10.0), (12.0, -40.0)] {
                for dt in [0.004, 0.05, 0.2] {
                    let exact = spring.solve(y, v, dt);
                    let reference = numeric(spring, y, v, dt);
                    assert!(
                        (exact.0 - reference.0).abs() < 1e-7
                            && (exact.1 - reference.1).abs() < 1e-5,
                        "{spring:?} y={y} v={v} dt={dt}: {exact:?} vs {reference:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn same_motion_at_any_frame_rate() {
        for spring in SPRINGS {
            let mut coarse = Motion {
                value: 10.0,
                velocity: -5.0,
                target: 0.0,
                spring,
            };
            let mut fine = coarse;
            coarse.step(0.03);
            for _ in 0..30 {
                fine.step(0.001);
            }
            assert!((coarse.value - fine.value).abs() < 1e-9, "{spring:?}");
            assert!((coarse.velocity - fine.velocity).abs() < 1e-7, "{spring:?}");
        }
    }

    /// Time until a unit step settles, and the largest overshoot on the way.
    fn run_step(spring: Spring) -> (f64, f64, f64) {
        let mut motion = Motion {
            value: 0.0,
            velocity: 0.0,
            target: 1.0,
            spring,
        };
        let (mut t, mut peak, mut ninety) = (0.0, 0.0f64, None);
        while !motion.resting() {
            motion.step(1.0 / 240.0);
            t += 1.0 / 240.0;
            peak = peak.max(motion.value);
            if ninety.is_none() && motion.value >= 0.9 {
                ninety = Some(t);
            }
            assert!(t < 2.0, "{spring:?} never settles");
        }
        (t, peak - 1.0, ninety.unwrap_or(t))
    }

    #[test]
    fn presets_are_fast_and_clear() {
        for spring in [Spring::GLIDE, Spring::FADE, Spring::POP] {
            let (settle, overshoot, ninety) = run_step(spring);
            assert!(ninety < 0.12, "{spring:?}: 90 % after {ninety:.3}s");
            assert!(settle < 0.32, "{spring:?}: at rest after {settle:.3}s");
            if spring.damping >= 1.0 {
                assert!(overshoot <= 1e-9, "{spring:?} overshoots {overshoot}");
            } else {
                assert!(
                    (0.02..0.09).contains(&overshoot),
                    "{spring:?} overshoot {overshoot}"
                );
            }
        }
        let (settle, overshoot, _) = run_step(Spring::COUNT);
        assert!(settle < 0.6 && overshoot <= 1e-9);
    }

    #[test]
    fn critically_damped_never_reverses() {
        let mut motion = Motion {
            value: 0.0,
            velocity: 0.0,
            target: 40.0,
            spring: Spring::GLIDE,
        };
        let mut last = motion.value;
        while !motion.resting() {
            motion.step(1.0 / 60.0);
            assert!(
                motion.value >= last - 1e-12,
                "{} after {}",
                motion.value,
                last
            );
            last = motion.value;
        }
        assert_eq!(motion.value, 40.0, "lands exactly");
    }

    #[test]
    fn retarget_keeps_velocity() {
        let mut motions = Motions::new(false);
        let t0 = Instant::now();
        motions.tick(t0);
        motions.follow(Key::Scroll, 0.0, Spring::GLIDE);
        motions.follow(Key::Scroll, 20.0, Spring::GLIDE);
        motions.tick(t0 + Duration::from_millis(16));
        motions.tick(t0 + Duration::from_millis(48));
        let before = motions.motions[&Key::Scroll];
        assert!(before.velocity > 0.0 && before.value > 0.0 && before.value < 20.0);
        motions.follow(Key::Scroll, -10.0, Spring::GLIDE);
        let after = motions.motions[&Key::Scroll];
        assert_eq!(
            (after.value, after.velocity),
            (before.value, before.velocity),
            "continuous"
        );
        // Right after the turn it still moves on (velocity is continuous)...
        motions.tick(t0 + Duration::from_millis(50));
        let next = motions.motions[&Key::Scroll];
        assert!(
            next.value > before.value,
            "momentum carries it on before turning"
        );
        // ...then heads for the new target.
        motions.tick(t0 + Duration::from_millis(66));
        let turned = motions.motions[&Key::Scroll];
        assert!(
            turned.value < next.value && turned.velocity < 0.0,
            "{turned:?}"
        );
    }

    #[test]
    fn follow_animates_changes_and_rests() {
        let mut motions = Motions::new(false);
        let t0 = Instant::now();
        motions.tick(t0);
        assert_eq!(
            motions.follow(Key::Context, 0.4, Spring::COUNT),
            0.4,
            "first value as is"
        );
        assert!(!motions.active());
        assert_eq!(
            motions.follow(Key::Context, 0.8, Spring::COUNT),
            0.4,
            "change starts here"
        );
        assert!(motions.active());
        let mut t = t0;
        let mut seen = Vec::new();
        for _ in 0..120 {
            t += Duration::from_millis(16);
            motions.tick(t);
            seen.push(motions.follow(Key::Context, 0.8, Spring::COUNT));
        }
        assert!(seen.windows(2).all(|w| w[1] >= w[0]), "monotonic");
        assert!(seen[2] > 0.4 && seen[2] < 0.8, "in between at frame 3");
        assert_eq!(*seen.last().unwrap(), 0.8);
        assert!(!motions.active());
    }

    #[test]
    fn a_frame_after_idle_does_not_skip_the_start() {
        let mut motions = Motions::new(false);
        let t0 = Instant::now();
        motions.tick(t0);
        motions.follow(Key::Notice, 1.0, Spring::FADE);
        // No frames for ten seconds, then one in which something appears.
        let shown = t0 + Duration::from_secs(10);
        motions.tick(shown);
        motions.start(Key::Notice, 0.0, 1.0, Spring::FADE);
        assert_eq!(
            motions.value(&Key::Notice),
            Some(0.0),
            "first frame shows the start"
        );
        motions.tick(shown + Duration::from_millis(16));
        let first = motions.value(&Key::Notice).unwrap();
        assert!(first > 0.0 && first < 0.5);
        // A long stall mid-motion advances at most one capped step.
        motions.tick(t0 + Duration::from_secs(20));
        let stalled = motions.value(&Key::Notice).unwrap();
        assert!(stalled < 1.0, "{stalled}: capped, not finished");
    }

    #[test]
    fn nudge_and_snap() {
        let mut motions = Motions::new(false);
        let t0 = Instant::now();
        motions.tick(t0);
        motions.follow(Key::Scroll, 0.0, Spring::GLIDE);
        motions.nudge(Key::Scroll, 3.0);
        assert_eq!(
            motions.value(&Key::Scroll),
            Some(3.0),
            "content stays where it was"
        );
        motions.tick(t0 + Duration::from_millis(16));
        let mid = motions.value(&Key::Scroll).unwrap();
        assert!(mid < 3.0 && mid > 0.0, "{mid}");
        motions.snap(Key::Scroll, 0.0);
        assert!(!motions.active());
        motions.nudge(Key::Context, 1.0);
        assert_eq!(
            motions.value(&Key::Context),
            None,
            "nudging nothing is a no-op"
        );
    }

    #[test]
    fn finished_rows_are_dropped() {
        let mut motions = Motions::new(false);
        let mut t = Instant::now();
        motions.tick(t);
        for row in 0..50 {
            motions.start(Key::Row(row), 0.0, 1.0, Spring::FADE);
        }
        for _ in 0..60 {
            t += Duration::from_millis(16);
            motions.tick(t);
        }
        assert!(motions.motions.is_empty(), "{} left", motions.motions.len());
        motions.start(Key::Row(1), 0.0, 1.0, Spring::FADE);
        motions.forget_rows();
        assert!(motions.motions.is_empty());
    }

    #[test]
    fn reduced_motion_snaps() {
        let mut motions = Motions::new(true);
        motions.tick(Instant::now());
        motions.follow(Key::Scroll, 0.0, Spring::GLIDE);
        assert_eq!(motions.follow(Key::Scroll, 30.0, Spring::GLIDE), 30.0);
        motions.start(Key::Row(4), 0.0, 1.0, Spring::FADE);
        assert_eq!(motions.value(&Key::Row(4)), Some(1.0));
        motions.nudge(Key::Scroll, 5.0);
        assert_eq!(motions.value(&Key::Scroll), Some(30.0));
        assert!(!motions.active());
    }

    #[test]
    fn colours_mix_per_channel() {
        assert_eq!(mix(0x000000, 0xffffff, 0.0), 0x000000);
        assert_eq!(mix(0x000000, 0xffffff, 1.0), 0xffffff);
        assert_eq!(mix(0x000000, 0xffffff, 0.5), 0x808080);
        assert_eq!(mix(0x102030, 0x102030, 0.37), 0x102030);
        assert_eq!(mix(0xff0000, 0x0000ff, 0.25), 0xbf0040);
        assert_eq!(mix(0x000000, 0xffffff, 1.4), 0xffffff, "overshoot clamps");
        assert_eq!(mix(0x000000, 0xffffff, -2.0), 0x000000);
    }

    #[test]
    fn rows_fade_tint_and_reveal() {
        let row = || {
            vec![
                run("a", Paint::fg(0xffffff)),
                run("b", Paint::fg(0x808080).on(0x404040)),
            ]
        };
        let mut gone = row();
        fade(&mut gone, 0x000000, 0.0);
        assert!(
            gone.iter()
                .all(|r| r.paint.fg == 0 && r.paint.bg.is_none_or(|bg| bg == 0))
        );
        let mut full = row();
        fade(&mut full, 0x000000, 1.0);
        assert_eq!(full, row());
        let mut half = row();
        fade(&mut half, 0x000000, 0.5);
        assert_eq!(
            (half[0].paint.fg, half[1].paint.bg),
            (0x808080, Some(0x202020))
        );
        assert_eq!(half[0].text, "a", "text untouched");

        let mut lit = row();
        tint(&mut lit, 0x000000, 0.5);
        assert_eq!(
            (lit[0].paint, lit[1].paint.bg),
            (row()[0].paint, Some(0x202020))
        );

        assert_eq!(reveal(6, 0.0), 0);
        assert_eq!(reveal(6, 0.01), 1);
        assert_eq!(reveal(6, 0.5), 3);
        assert_eq!(reveal(6, 1.08), 6, "overshoot shows all, never more");
        assert_eq!(reveal(0, 1.0), 0);
    }
}
