//! Fixed-timestep accumulation.
//!
//! [`FixedTimestep`] decouples a simulation's update rate from the render
//! frame rate: each frame you hand it the real elapsed time, and it tells
//! you how many whole fixed-size steps to run now, keeping the leftover
//! for next frame. Physics, and any other subsystem that needs
//! reproducible, frame-rate-independent stepping, drives its update loop
//! from this instead of the variable per-frame delta.

use crate::error::EngineError;

/// Default step rate when none is given: 60 steps per second.
const DEFAULT_HZ: f32 = 60.0;

/// Default cap on steps run per [`FixedTimestep::advance`] call — enough
/// catch-up for a ~130 ms stall at 60 Hz before the simulation
/// deliberately falls behind wall-clock rather than spiralling.
const DEFAULT_MAX_STEPS: u32 = 8;

/// An accumulator that converts a stream of variable frame deltas into a
/// count of fixed-size simulation steps.
///
/// Feed each frame's real elapsed seconds to [`FixedTimestep::advance`];
/// it returns how many steps of [`FixedTimestep::step_seconds`] to run
/// this frame and carries the sub-step remainder forward. The remainder,
/// as a `0.0..=1.0` fraction, is available from [`FixedTimestep::alpha`]
/// for interpolating rendered poses between the last two steps.
///
/// # Spiral-of-death guard
///
/// If the accumulated time would require more than
/// [`FixedTimestep::max_steps_per_advance`] steps in one call (a long
/// stall, or a machine that simply can't simulate fast enough), the
/// excess is discarded: `advance` returns the cap and the simulation runs
/// slow rather than requesting an ever-growing step count it can never
/// work off.
///
/// # Example
///
/// ```
/// use engine_core::FixedTimestep;
///
/// let mut ticker = FixedTimestep::from_hz(50.0)?; // 0.02 s per step
/// assert_eq!(ticker.advance(0.05), 2);            // 0.05 s -> 2 steps, 0.01 s left
/// assert!((ticker.accumulator() - 0.01).abs() < 1e-6);
/// assert_eq!(ticker.advance(0.005), 0);           // 0.015 s total, still < one step
/// assert_eq!(ticker.advance(0.005), 1);           // 0.02 s -> 1 step
/// # Ok::<(), engine_core::EngineError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FixedTimestep {
    step_seconds: f32,
    accumulator: f32,
    max_steps_per_advance: u32,
}

impl FixedTimestep {
    /// A ticker running at `hz` steps per second.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::InvalidTimestep`] if `hz` is not finite and
    /// positive.
    pub fn from_hz(hz: f32) -> Result<Self, EngineError> {
        if !(hz.is_finite() && hz > 0.0) {
            return Err(EngineError::InvalidTimestep(hz));
        }
        Self::from_seconds(1.0 / hz)
    }

    /// A ticker whose fixed step is `step_seconds` long.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::InvalidTimestep`] if `step_seconds` is not
    /// finite and positive.
    pub fn from_seconds(step_seconds: f32) -> Result<Self, EngineError> {
        if !(step_seconds.is_finite() && step_seconds > 0.0) {
            return Err(EngineError::InvalidTimestep(step_seconds));
        }
        Ok(Self {
            step_seconds,
            accumulator: 0.0,
            max_steps_per_advance: DEFAULT_MAX_STEPS,
        })
    }

    /// Sets the cap on steps returned by a single [`FixedTimestep::advance`]
    /// call. Builder-style. A value of `0` is raised to `1` — `advance`
    /// must always be able to make progress.
    #[must_use]
    pub fn with_max_steps(mut self, max_steps: u32) -> Self {
        self.max_steps_per_advance = max_steps.max(1);
        self
    }

    /// The length of one fixed step, in seconds.
    pub fn step_seconds(&self) -> f32 {
        self.step_seconds
    }

    /// The most steps a single [`FixedTimestep::advance`] call will report.
    pub fn max_steps_per_advance(&self) -> u32 {
        self.max_steps_per_advance
    }

    /// Unconsumed time carried toward the next step, in seconds — always
    /// in `0.0..step_seconds()` after an [`FixedTimestep::advance`].
    pub fn accumulator(&self) -> f32 {
        self.accumulator
    }

    /// The accumulator as a `0.0..=1.0` fraction of one step: how far the
    /// simulation is from the *next* step, for interpolating rendered
    /// state between the previous and current step's poses.
    pub fn alpha(&self) -> f32 {
        (self.accumulator / self.step_seconds).clamp(0.0, 1.0)
    }

    /// Discards any accumulated time. Call after a deliberate
    /// discontinuity — loading a scene, unpausing — so the stall doesn't
    /// register as simulation time to catch up on.
    pub fn reset(&mut self) {
        self.accumulator = 0.0;
    }

    /// Adds `frame_seconds` of real elapsed time and returns how many
    /// fixed steps to run now, subtracting their time from the
    /// accumulator.
    ///
    /// A negative, `NaN`, or infinite `frame_seconds` contributes `0.0`
    /// rather than erroring — `advance` runs every frame, and a bad delta
    /// from a clock glitch should stall the simulation for a frame, not
    /// take down the loop. The return value never exceeds
    /// [`FixedTimestep::max_steps_per_advance`]; see the type docs for
    /// what happens to the excess.
    pub fn advance(&mut self, frame_seconds: f32) -> u32 {
        let delta = if frame_seconds.is_finite() && frame_seconds >= 0.0 {
            frame_seconds
        } else {
            0.0
        };
        self.accumulator += delta;

        // Cap first: bounds the step count (so the `as u32` below can't
        // overflow) and drops backlog the simulation can't work off.
        let ceiling = self.max_steps_per_advance as f32 * self.step_seconds;
        if self.accumulator > ceiling {
            self.accumulator = ceiling;
        }

        let steps = (self.accumulator / self.step_seconds).floor() as u32;
        self.accumulator -= steps as f32 * self.step_seconds;
        // Floating-point subtraction can leave a tiny negative residue.
        if self.accumulator < 0.0 {
            self.accumulator = 0.0;
        }
        steps
    }
}

impl Default for FixedTimestep {
    /// 60 steps per second, at most 8 steps per [`FixedTimestep::advance`].
    fn default() -> Self {
        Self {
            step_seconds: 1.0 / DEFAULT_HZ,
            accumulator: 0.0,
            max_steps_per_advance: DEFAULT_MAX_STEPS,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_hz_sets_the_reciprocal_step() {
        let ticker = FixedTimestep::from_hz(60.0).unwrap();
        assert!((ticker.step_seconds() - 1.0 / 60.0).abs() < 1e-9);
    }

    #[test]
    fn from_hz_rejects_non_positive_and_non_finite() {
        assert!(FixedTimestep::from_hz(0.0).is_err());
        assert!(FixedTimestep::from_hz(-30.0).is_err());
        assert!(FixedTimestep::from_hz(f32::NAN).is_err());
        assert!(FixedTimestep::from_hz(f32::INFINITY).is_err());
    }

    #[test]
    fn from_seconds_rejects_non_positive_and_non_finite() {
        assert!(FixedTimestep::from_seconds(0.0).is_err());
        assert!(FixedTimestep::from_seconds(-0.016).is_err());
        assert!(FixedTimestep::from_seconds(f32::NAN).is_err());
        assert!(FixedTimestep::from_seconds(f32::NEG_INFINITY).is_err());
    }

    #[test]
    fn invalid_timestep_error_carries_the_offending_value() {
        let err = FixedTimestep::from_hz(-5.0).unwrap_err();
        assert!(matches!(err, EngineError::InvalidTimestep(hz) if hz == -5.0));
    }

    #[test]
    fn default_is_sixty_hz_eight_steps() {
        let ticker = FixedTimestep::default();
        assert!((ticker.step_seconds() - 1.0 / 60.0).abs() < 1e-9);
        assert_eq!(ticker.max_steps_per_advance(), 8);
        assert_eq!(ticker.accumulator(), 0.0);
    }

    #[test]
    fn with_max_steps_raises_zero_to_one() {
        let ticker = FixedTimestep::default().with_max_steps(0);
        assert_eq!(ticker.max_steps_per_advance(), 1);
    }

    #[test]
    fn advance_by_exactly_one_step_returns_one_and_empties_the_accumulator() {
        let mut ticker = FixedTimestep::from_seconds(0.5).unwrap();
        assert_eq!(ticker.advance(0.5), 1);
        assert_eq!(ticker.accumulator(), 0.0);
    }

    #[test]
    fn advance_accumulates_across_calls() {
        let mut ticker = FixedTimestep::from_seconds(0.5).unwrap();
        assert_eq!(ticker.advance(0.25), 0);
        assert_eq!(ticker.advance(0.25), 1);
        assert_eq!(ticker.accumulator(), 0.0);
    }

    #[test]
    fn advance_returns_multiple_steps_and_keeps_the_remainder() {
        let mut ticker = FixedTimestep::from_seconds(0.25).unwrap();
        assert_eq!(ticker.advance(0.8), 3); // 0.75 consumed
        assert!((ticker.accumulator() - 0.05).abs() < 1e-6);
    }

    #[test]
    fn advance_caps_steps_and_discards_backlog() {
        let mut ticker = FixedTimestep::from_hz(60.0).unwrap().with_max_steps(8);
        assert_eq!(ticker.advance(10.0), 8);
        // Backlog dropped: accumulator left below a single step.
        assert!(ticker.accumulator() < ticker.step_seconds());
    }

    #[test]
    fn advance_ignores_negative_nan_and_infinite_deltas() {
        let mut ticker = FixedTimestep::from_seconds(0.1).unwrap();
        ticker.advance(0.05);
        let before = ticker.accumulator();
        assert_eq!(ticker.advance(-1.0), 0);
        assert_eq!(ticker.advance(f32::NAN), 0);
        assert_eq!(ticker.advance(f32::INFINITY), 0);
        assert_eq!(ticker.accumulator(), before);
    }

    #[test]
    fn alpha_reports_the_sub_step_fraction() {
        let mut ticker = FixedTimestep::from_seconds(0.2).unwrap();
        ticker.advance(0.1); // half a step
        assert!((ticker.alpha() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn alpha_is_clamped_to_the_unit_range() {
        let ticker = FixedTimestep::from_seconds(0.1).unwrap();
        assert_eq!(ticker.alpha(), 0.0);
        // Even a mid-advance accumulator never reads above 1.0.
        let mut t = FixedTimestep::from_seconds(0.1).unwrap().with_max_steps(4);
        t.advance(1.0);
        assert!(t.alpha() <= 1.0);
    }

    #[test]
    fn reset_drops_the_accumulator() {
        let mut ticker = FixedTimestep::from_seconds(0.5).unwrap();
        ticker.advance(0.3);
        assert!(ticker.accumulator() > 0.0);
        ticker.reset();
        assert_eq!(ticker.accumulator(), 0.0);
    }

    #[test]
    fn steady_feed_produces_one_step_per_frame_without_drift() {
        // Exact-in-f32 values: 0.5 s step, quarter-step frames.
        let mut ticker = FixedTimestep::from_seconds(0.5).unwrap();
        let mut total = 0u32;
        for _ in 0..2000 {
            total += ticker.advance(0.25);
        }
        // 2000 quarter-steps == 1000 whole steps, exactly.
        assert_eq!(total, 1000);
        assert_eq!(ticker.accumulator(), 0.0);
    }
}
