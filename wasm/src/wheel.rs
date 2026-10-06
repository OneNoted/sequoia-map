//! Mouse-wheel and trackpad zoom input.
//!
//! Browsers report wheels, precision trackpads and pinch-to-zoom gestures all as `wheel`
//! events. These helpers tell them apart and turn each into a zoom delta for
//! [`Viewport::zoom_at`](crate::viewport::Viewport::zoom_at).

const WHEEL_DELTA_MODE_PIXEL: u32 = 0;
const WHEEL_DELTA_MODE_LINE: u32 = 1;
const WHEEL_DELTA_MODE_PAGE: u32 = 2;
const TRACKPAD_BURST_GAP_MS: f64 = 45.0;
const TRACKPAD_STICKY_MS: f64 = 900.0;
const TRACKPAD_STICKY_CONTINUE_GAP_MS: f64 = 180.0;
const TRACKPAD_STICKY_PIXEL_DELTA_LIMIT: f64 = 96.0;
const TRACKPAD_SMALL_PIXEL_DELTA: f64 = 32.0;
const TRACKPAD_BURST_DELTA_LIMIT: f64 = 80.0;
const TRACKPAD_LINE_HEIGHT_PX: f64 = 18.0;
const TRACKPAD_PAGE_HEIGHT_FACTOR: f64 = 0.9;
const TRACKPAD_ZOOM_GAIN: f64 = 2.35;
const TRACKPAD_ZOOM_CLAMP: f64 = 280.0;
const PINCH_LINE_HEIGHT_PX: f64 = 24.0;
const PINCH_PAGE_HEIGHT_FACTOR: f64 = 1.0;
const PINCH_ZOOM_GAIN: f64 = 4.2;
const PINCH_ZOOM_CLAMP: f64 = 420.0;

/// One `wheel` event, as reported by the browser.
#[derive(Clone, Copy, Debug)]
pub struct WheelSample {
    pub delta_x: f64,
    pub delta_y: f64,
    /// `WheelEvent.deltaMode`: 0 pixels, 1 lines, 2 pages.
    pub delta_mode: u32,
    pub timestamp_ms: f64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TrackpadWheelClassifier {
    last_event_ms: f64,
    rapid_streak: u8,
    trackpad_until_ms: f64,
}

impl Default for TrackpadWheelClassifier {
    fn default() -> Self {
        Self {
            last_event_ms: -1.0,
            rapid_streak: 0,
            trackpad_until_ms: 0.0,
        }
    }
}

impl TrackpadWheelClassifier {
    fn is_trackpad(&mut self, sample: WheelSample) -> bool {
        let elapsed_ms = if self.last_event_ms >= 0.0 && sample.timestamp_ms > self.last_event_ms {
            sample.timestamp_ms - self.last_event_ms
        } else {
            f64::INFINITY
        };

        let rapid = elapsed_ms <= TRACKPAD_BURST_GAP_MS;
        self.rapid_streak = if rapid {
            self.rapid_streak.saturating_add(1)
        } else {
            1
        };

        let abs_x = sample.delta_x.abs();
        let abs_y = sample.delta_y.abs();
        let fractional = has_fractional_component(abs_x) || has_fractional_component(abs_y);
        let small_pixel_delta = sample.delta_mode == WHEEL_DELTA_MODE_PIXEL
            && abs_y > 0.0
            && abs_y <= TRACKPAD_SMALL_PIXEL_DELTA;
        let mixed_axis_scroll = sample.delta_mode == WHEEL_DELTA_MODE_PIXEL
            && abs_x > 0.0
            && abs_y > 0.0
            && abs_x <= TRACKPAD_BURST_DELTA_LIMIT
            && abs_y <= TRACKPAD_BURST_DELTA_LIMIT;
        let bursty_precision_scroll = sample.delta_mode == WHEEL_DELTA_MODE_PIXEL
            && self.rapid_streak >= 4
            && abs_y > 0.0
            && abs_y <= TRACKPAD_BURST_DELTA_LIMIT;

        let has_direct_trackpad_signal =
            fractional || small_pixel_delta || mixed_axis_scroll || bursty_precision_scroll;
        let sticky_continuation = self.trackpad_until_ms > 0.0
            && sample.timestamp_ms <= self.trackpad_until_ms
            && sample.delta_mode == WHEEL_DELTA_MODE_PIXEL
            && elapsed_ms <= TRACKPAD_STICKY_CONTINUE_GAP_MS
            && abs_y > 0.0
            && abs_y <= TRACKPAD_STICKY_PIXEL_DELTA_LIMIT;
        let is_trackpad = has_direct_trackpad_signal || sticky_continuation;

        self.last_event_ms = sample.timestamp_ms;
        if is_trackpad {
            self.trackpad_until_ms = sample.timestamp_ms + TRACKPAD_STICKY_MS;
        }

        is_trackpad
    }
}

fn has_fractional_component(value: f64) -> bool {
    let rounded = value.round();
    (value - rounded).abs() > 0.01
}

fn normalize_trackpad_zoom_delta(sample: WheelSample, viewport_height: f64) -> f64 {
    let raw_pixels = match sample.delta_mode {
        WHEEL_DELTA_MODE_PIXEL => sample.delta_y,
        WHEEL_DELTA_MODE_LINE => sample.delta_y * TRACKPAD_LINE_HEIGHT_PX,
        WHEEL_DELTA_MODE_PAGE => {
            sample.delta_y * viewport_height.max(1.0) * TRACKPAD_PAGE_HEIGHT_FACTOR
        }
        _ => sample.delta_y,
    };
    (raw_pixels * TRACKPAD_ZOOM_GAIN).clamp(-TRACKPAD_ZOOM_CLAMP, TRACKPAD_ZOOM_CLAMP)
}

fn normalize_pinch_zoom_delta(sample: WheelSample, viewport_height: f64) -> f64 {
    let raw_pixels = match sample.delta_mode {
        WHEEL_DELTA_MODE_PIXEL => sample.delta_y,
        WHEEL_DELTA_MODE_LINE => sample.delta_y * PINCH_LINE_HEIGHT_PX,
        WHEEL_DELTA_MODE_PAGE => {
            sample.delta_y * viewport_height.max(1.0) * PINCH_PAGE_HEIGHT_FACTOR
        }
        _ => sample.delta_y,
    };
    (raw_pixels * PINCH_ZOOM_GAIN).clamp(-PINCH_ZOOM_CLAMP, PINCH_ZOOM_CLAMP)
}

fn has_trackpad_like_ctrl_pinch_signal(sample: WheelSample) -> bool {
    let abs_x = sample.delta_x.abs();
    let abs_y = sample.delta_y.abs();
    let fractional = has_fractional_component(abs_x) || has_fractional_component(abs_y);
    let small_or_mid_pixel_delta = sample.delta_mode == WHEEL_DELTA_MODE_PIXEL
        && abs_y > 0.0
        && abs_y <= TRACKPAD_STICKY_PIXEL_DELTA_LIMIT;
    let mixed_axis_scroll = sample.delta_mode == WHEEL_DELTA_MODE_PIXEL
        && abs_x > 0.0
        && abs_y > 0.0
        && abs_x <= TRACKPAD_BURST_DELTA_LIMIT
        && abs_y <= TRACKPAD_BURST_DELTA_LIMIT;
    fractional || small_or_mid_pixel_delta || mixed_axis_scroll
}

pub(crate) fn normalize_wheel_zoom_delta(
    sample: WheelSample,
    viewport_height: f64,
    ctrl_pinch: bool,
    classifier: &mut TrackpadWheelClassifier,
) -> f64 {
    let is_trackpad = classifier.is_trackpad(sample);
    if ctrl_pinch && (is_trackpad || has_trackpad_like_ctrl_pinch_signal(sample)) {
        normalize_pinch_zoom_delta(sample, viewport_height)
    } else if is_trackpad {
        normalize_trackpad_zoom_delta(sample, viewport_height)
    } else {
        sample.delta_y
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PINCH_LINE_HEIGHT_PX, PINCH_ZOOM_GAIN, TRACKPAD_LINE_HEIGHT_PX,
        TRACKPAD_PAGE_HEIGHT_FACTOR, TRACKPAD_ZOOM_CLAMP, TRACKPAD_ZOOM_GAIN,
        TrackpadWheelClassifier, WHEEL_DELTA_MODE_LINE, WHEEL_DELTA_MODE_PAGE,
        WHEEL_DELTA_MODE_PIXEL, WheelSample, normalize_pinch_zoom_delta,
        normalize_trackpad_zoom_delta, normalize_wheel_zoom_delta,
    };

    fn sample(delta_y: f64, delta_mode: u32, timestamp_ms: f64) -> WheelSample {
        WheelSample {
            delta_x: 0.0,
            delta_y,
            delta_mode,
            timestamp_ms,
        }
    }

    fn assert_close(actual: f64, expected: f64) {
        let diff = (actual - expected).abs();
        assert!(
            diff < 1e-9,
            "expected {expected}, got {actual} (diff: {diff})"
        );
    }

    #[test]
    fn keeps_discrete_mouse_wheel_on_legacy_path() {
        let mut classifier = TrackpadWheelClassifier::default();
        let first = sample(100.0, WHEEL_DELTA_MODE_PIXEL, 0.0);
        let second = sample(100.0, WHEEL_DELTA_MODE_PIXEL, 130.0);

        assert!(!classifier.is_trackpad(first));
        assert!(!classifier.is_trackpad(second));
    }

    #[test]
    fn detects_fractional_line_input_as_trackpad() {
        let mut classifier = TrackpadWheelClassifier::default();
        let s = sample(0.35, WHEEL_DELTA_MODE_LINE, 0.0);

        assert!(classifier.is_trackpad(s));
    }

    #[test]
    fn detects_windows_precision_trackpad_bursts() {
        let mut classifier = TrackpadWheelClassifier::default();
        let stream = [
            sample(60.0, WHEEL_DELTA_MODE_PIXEL, 0.0),
            sample(58.0, WHEEL_DELTA_MODE_PIXEL, 16.0),
            sample(61.0, WHEEL_DELTA_MODE_PIXEL, 32.0),
            sample(57.0, WHEEL_DELTA_MODE_PIXEL, 48.0),
        ];

        assert!(!classifier.is_trackpad(stream[0]));
        assert!(!classifier.is_trackpad(stream[1]));
        assert!(!classifier.is_trackpad(stream[2]));
        assert!(classifier.is_trackpad(stream[3]));
    }

    #[test]
    fn keeps_trackpad_classification_during_momentum_tail() {
        let mut classifier = TrackpadWheelClassifier::default();
        assert!(classifier.is_trackpad(sample(0.5, WHEEL_DELTA_MODE_LINE, 10.0)));

        let momentum_tail = sample(90.0, WHEEL_DELTA_MODE_PIXEL, 120.0);
        assert!(classifier.is_trackpad(momentum_tail));
    }

    #[test]
    fn sticky_window_does_not_reclassify_mouse_line_wheel() {
        let mut classifier = TrackpadWheelClassifier::default();
        assert!(classifier.is_trackpad(sample(0.5, WHEEL_DELTA_MODE_LINE, 10.0)));

        let mouse_wheel_event = sample(3.0, WHEEL_DELTA_MODE_LINE, 80.0);
        assert!(!classifier.is_trackpad(mouse_wheel_event));
    }

    #[test]
    fn sticky_window_does_not_reclassify_large_pixel_mouse_ticks() {
        let mut classifier = TrackpadWheelClassifier::default();
        assert!(classifier.is_trackpad(sample(0.4, WHEEL_DELTA_MODE_LINE, 10.0)));

        let mouse_wheel_event = sample(120.0, WHEEL_DELTA_MODE_PIXEL, 90.0);
        assert!(!classifier.is_trackpad(mouse_wheel_event));
    }

    #[test]
    fn normalizes_line_deltas_to_pixel_zoom_rate() {
        let s = sample(0.5, WHEEL_DELTA_MODE_LINE, 0.0);
        let normalized = normalize_trackpad_zoom_delta(s, 800.0);
        let expected = 0.5 * TRACKPAD_LINE_HEIGHT_PX * TRACKPAD_ZOOM_GAIN;
        assert_close(normalized, expected);
    }

    #[test]
    fn clamps_large_page_deltas() {
        let s = sample(1.0, WHEEL_DELTA_MODE_PAGE, 0.0);
        let normalized = normalize_trackpad_zoom_delta(s, 1000.0);
        let unclamped = 1000.0 * TRACKPAD_PAGE_HEIGHT_FACTOR * TRACKPAD_ZOOM_GAIN;
        assert!(unclamped > TRACKPAD_ZOOM_CLAMP);
        assert_close(normalized, TRACKPAD_ZOOM_CLAMP);
    }

    #[test]
    fn ctrl_wheel_uses_pinch_normalization() {
        let mut classifier = TrackpadWheelClassifier::default();
        let s = sample(0.5, WHEEL_DELTA_MODE_LINE, 0.0);
        let normalized = normalize_wheel_zoom_delta(s, 800.0, true, &mut classifier);
        let expected = 0.5 * PINCH_LINE_HEIGHT_PX * PINCH_ZOOM_GAIN;
        assert_close(normalized, expected);
    }

    #[test]
    fn ctrl_line_mode_mouse_wheel_keeps_legacy_delta() {
        let mut classifier = TrackpadWheelClassifier::default();
        let s = sample(3.0, WHEEL_DELTA_MODE_LINE, 0.0);
        let normalized = normalize_wheel_zoom_delta(s, 800.0, true, &mut classifier);
        assert_close(normalized, 3.0);
    }

    #[test]
    fn ctrl_large_pixel_mouse_wheel_keeps_legacy_delta() {
        let mut classifier = TrackpadWheelClassifier::default();
        let s = sample(120.0, WHEEL_DELTA_MODE_PIXEL, 0.0);
        let normalized = normalize_wheel_zoom_delta(s, 800.0, true, &mut classifier);
        assert_close(normalized, 120.0);
    }

    #[test]
    fn discrete_mouse_wheel_keeps_legacy_delta_without_ctrl() {
        let mut classifier = TrackpadWheelClassifier::default();
        let s = sample(120.0, WHEEL_DELTA_MODE_LINE, 0.0);
        let normalized = normalize_wheel_zoom_delta(s, 800.0, false, &mut classifier);
        assert_close(normalized, 120.0);
    }

    #[test]
    fn pinch_normalization_scales_line_mode_aggressively() {
        let s = sample(-0.75, WHEEL_DELTA_MODE_LINE, 0.0);
        let normalized = normalize_pinch_zoom_delta(s, 1000.0);
        let expected = -0.75 * PINCH_LINE_HEIGHT_PX * PINCH_ZOOM_GAIN;
        assert_close(normalized, expected);
    }
}
