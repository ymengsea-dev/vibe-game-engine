//! A lightweight, dependency-free CPU frame profiler and its panel.
//!
//! The editor loop calls [`FrameProfiler::begin_frame`], then
//! [`FrameProfiler::record_span`] for each timed section (measured with
//! `std::time::Instant` at the call site — no closure API, so timing a
//! section never conflicts with borrowing the rest of the editor state),
//! then [`FrameProfiler::end_frame`]. [`show`] draws the rolling
//! average/peak, an FPS estimate, a sparkline of recent frame times, and
//! the most recent frame's per-section breakdown.
//!
//! CPU only — GPU-side timing needs `wgpu` timestamp queries and is
//! future work. A `puffin`/`tracy` integration could replace this
//! wholesale later; this keeps the editor honest about frame cost with no
//! extra dependency.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Frames of history kept for the rolling stats and sparkline.
const HISTORY_LEN: usize = 180;

/// One completed frame's timing: total wall time plus each recorded
/// section, in milliseconds.
#[derive(Debug, Clone, Default)]
pub struct FrameSample {
    /// Wall-clock time from `begin_frame` to `end_frame`.
    pub total_ms: f32,
    /// `(name, milliseconds)` per [`FrameProfiler::record_span`] call,
    /// in call order.
    pub spans: Vec<(String, f32)>,
    /// Asynchronous GPU duration, when the adapter supports timestamps.
    pub gpu_ms: Option<f32>,
}

/// Rolling CPU frame-time history.
pub struct FrameProfiler {
    frames: VecDeque<FrameSample>,
    frame_start: Instant,
    current_spans: Vec<(String, f32)>,
    pending_gpu_ms: Option<f32>,
}

impl Default for FrameProfiler {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameProfiler {
    /// An empty profiler.
    pub fn new() -> Self {
        Self {
            frames: VecDeque::with_capacity(HISTORY_LEN),
            frame_start: Instant::now(),
            current_spans: Vec::new(),
            pending_gpu_ms: None,
        }
    }

    /// Marks the start of a frame and clears the previous frame's
    /// in-progress spans.
    pub fn begin_frame(&mut self) {
        self.frame_start = Instant::now();
        self.current_spans.clear();
    }

    /// Records that section `name` took `duration`. Time the section with
    /// `std::time::Instant` at the call site and pass its `elapsed()`.
    pub fn record_span(&mut self, name: &str, duration: Duration) {
        self.current_spans
            .push((name.to_string(), duration.as_secs_f32() * 1000.0));
    }

    /// Supplies a completed asynchronous GPU result for the next frame.
    /// Invalid values are ignored, so unsupported adapters naturally fall
    /// back to CPU-only profiling without blocking the UI thread.
    pub fn record_gpu_ms(&mut self, milliseconds: f32) {
        if milliseconds.is_finite() && milliseconds >= 0.0 {
            self.pending_gpu_ms = Some(milliseconds);
        }
    }

    /// Closes the current frame: pushes a [`FrameSample`] with the total
    /// elapsed time and the recorded spans, dropping the oldest sample
    /// once history is full.
    pub fn end_frame(&mut self) {
        let sample = FrameSample {
            total_ms: self.frame_start.elapsed().as_secs_f32() * 1000.0,
            spans: std::mem::take(&mut self.current_spans),
            gpu_ms: self.pending_gpu_ms.take(),
        };
        if self.frames.len() == HISTORY_LEN {
            self.frames.pop_front();
        }
        self.frames.push_back(sample);
    }

    /// The most recently completed frame, if any.
    pub fn latest(&self) -> Option<&FrameSample> {
        self.frames.back()
    }

    /// Mean total frame time over the kept history, in ms (`0.0` if
    /// empty).
    pub fn average_ms(&self) -> f32 {
        if self.frames.is_empty() {
            return 0.0;
        }
        let sum: f32 = self.frames.iter().map(|frame| frame.total_ms).sum();
        sum / self.frames.len() as f32
    }

    /// Slowest total frame time over the kept history, in ms (`0.0` if
    /// empty).
    pub fn max_ms(&self) -> f32 {
        self.frames
            .iter()
            .map(|frame| frame.total_ms)
            .fold(0.0, f32::max)
    }

    /// Per-frame total times, oldest first — for the sparkline.
    pub fn totals(&self) -> impl Iterator<Item = f32> + '_ {
        self.frames.iter().map(|frame| frame.total_ms)
    }

    /// Number of frames currently in history.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Whether no frames have been recorded yet.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

/// Draws the profiler panel: FPS, average/peak CPU frame time, a
/// sparkline of recent frame times, and the latest frame's per-section
/// breakdown.
pub fn show(ui: &mut egui::Ui, profiler: &FrameProfiler) {
    let average = profiler.average_ms();
    let fps = if average > f32::EPSILON {
        1000.0 / average
    } else {
        0.0
    };

    ui.label(format!(
        "CPU: {average:.2} ms avg  ·  {:.2} ms peak  ·  ~{fps:.0} FPS",
        profiler.max_ms()
    ));
    if let Some(gpu_ms) = profiler.latest().and_then(|sample| sample.gpu_ms) {
        ui.label(format!("GPU: {gpu_ms:.2} ms (async)"));
    } else {
        ui.weak("GPU: unavailable (CPU timing remains active)");
    }

    sparkline(ui, profiler);

    if let Some(latest) = profiler.latest() {
        ui.separator();
        ui.label(format!("Last frame: {:.2} ms", latest.total_ms));
        for (name, ms) in &latest.spans {
            ui.label(format!("  {name}: {ms:.2} ms"));
        }
    }
}

/// A small bar-per-frame plot of recent total frame times, scaled to the
/// slowest frame in view.
fn sparkline(ui: &mut egui::Ui, profiler: &FrameProfiler) {
    let desired = egui::vec2(ui.available_width().min(360.0), 40.0);
    let (rect, _response) = ui.allocate_exact_size(desired, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);

    let peak = profiler.max_ms().max(1.0);
    let count = profiler.len().max(1);
    let bar_width = rect.width() / count as f32;

    for (index, total_ms) in profiler.totals().enumerate() {
        let height = (total_ms / peak).clamp(0.0, 1.0) * rect.height();
        let x = rect.left() + index as f32 * bar_width;
        let bar = egui::Rect::from_min_max(
            egui::pos2(x, rect.bottom() - height),
            egui::pos2(x + bar_width.max(1.0), rect.bottom()),
        );
        // Green under 16.7 ms (60 FPS), amber to 33 ms, red beyond.
        let color = if total_ms < 16.7 {
            egui::Color32::from_rgb(80, 200, 120)
        } else if total_ms < 33.3 {
            egui::Color32::from_rgb(230, 190, 90)
        } else {
            egui::Color32::from_rgb(220, 90, 90)
        };
        painter.rect_filled(bar, 0.0, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn new_profiler_is_empty() {
        let profiler = FrameProfiler::new();
        assert!(profiler.is_empty());
        assert_eq!(profiler.average_ms(), 0.0);
        assert_eq!(profiler.max_ms(), 0.0);
        assert!(profiler.latest().is_none());
    }

    #[test]
    fn end_frame_records_a_sample_with_its_spans() {
        let mut profiler = FrameProfiler::new();
        profiler.begin_frame();
        profiler.record_span("ui", span(2));
        profiler.record_span("render", span(3));
        profiler.end_frame();

        let latest = profiler.latest().unwrap();
        assert_eq!(latest.spans.len(), 2);
        assert_eq!(latest.spans[0].0, "ui");
        assert!((latest.spans[1].1 - 3.0).abs() < 1.0);
    }

    #[test]
    fn begin_frame_clears_the_previous_frames_spans() {
        let mut profiler = FrameProfiler::new();
        profiler.begin_frame();
        profiler.record_span("a", span(1));
        profiler.end_frame();

        profiler.begin_frame();
        profiler.record_span("b", span(1));
        profiler.end_frame();

        assert_eq!(profiler.latest().unwrap().spans.len(), 1);
        assert_eq!(profiler.latest().unwrap().spans[0].0, "b");
    }

    #[test]
    fn history_is_capped_at_history_len() {
        let mut profiler = FrameProfiler::new();
        for _ in 0..(HISTORY_LEN + 50) {
            profiler.begin_frame();
            profiler.end_frame();
        }
        assert_eq!(profiler.len(), HISTORY_LEN);
    }

    #[test]
    fn average_and_max_reflect_recorded_totals() {
        let mut profiler = FrameProfiler::new();
        // Can't control the wall clock precisely, but three quick frames
        // should all be well under a millisecond, so avg/max stay small
        // and finite.
        for _ in 0..3 {
            profiler.begin_frame();
            profiler.end_frame();
        }
        assert!(profiler.average_ms() >= 0.0 && profiler.average_ms() < 100.0);
        assert!(profiler.max_ms() >= profiler.average_ms());
    }

    #[test]
    fn gpu_results_are_async_and_invalid_values_are_ignored() {
        let mut profiler = FrameProfiler::new();
        profiler.record_gpu_ms(f32::NAN);
        profiler.begin_frame();
        profiler.end_frame();
        assert_eq!(profiler.latest().unwrap().gpu_ms, None);
        profiler.record_gpu_ms(4.5);
        profiler.begin_frame();
        profiler.end_frame();
        assert_eq!(profiler.latest().unwrap().gpu_ms, Some(4.5));
    }
}
