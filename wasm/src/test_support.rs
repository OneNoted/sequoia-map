//! Assertions the crate's unit tests share.

/// Equal up to floating-point noise.
pub(crate) fn assert_close(actual: f64, expected: f64) {
    let diff = (actual - expected).abs();
    assert!(
        diff < 1e-9,
        "expected {expected}, got {actual} (diff: {diff})"
    );
}
