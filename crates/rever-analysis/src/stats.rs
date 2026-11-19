//! Port of `src/main/mcp/stats.ts`.

/// Nearest-rank percentile of a pre-sorted (ascending) numeric array.
///
/// A `p` below 0 or NaN reads outside the array in the TypeScript and yields
/// `undefined`; here that is NaN.
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    let Some(last) = sorted.len().checked_sub(1) else {
        return 0.0;
    };
    let idx = (last as f64 * p).floor();
    if idx.is_nan() || idx < 0.0 {
        return f64::NAN;
    }
    // The cast saturates, so an oversized `p` lands on the last element.
    sorted
        .get(last.min(idx as usize))
        .copied()
        .unwrap_or(f64::NAN)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The case of src/main/mcp/pure-helpers.test.ts.
    #[test]
    fn 정렬된_배열에서_최근접_순위를_쓴다() {
        let s = [10.0, 20.0, 30.0, 40.0, 50.0];
        assert_eq!(percentile(&s, 0.5), 30.0);
        assert_eq!(percentile(&s, 0.95), 40.0); // floor((5-1)*0.95)=3 → 40
        assert_eq!(percentile(&s, 1.0), 50.0);
        assert_eq!(percentile(&[], 0.5), 0.0);
    }

    #[test]
    fn 빈_배열의_백분위수는_0이다() {
        assert_eq!(percentile(&[], 0.5), 0.0);
    }

    #[test]
    fn 최근접_순위로_백분위수를_고른다() {
        let sorted = [10.0, 20.0, 30.0, 40.0, 50.0];
        assert_eq!(percentile(&sorted, 0.0), 10.0);
        assert_eq!(percentile(&sorted, 0.5), 30.0);
        // floor(4 * 0.95) = 3
        assert_eq!(percentile(&sorted, 0.95), 40.0);
        assert_eq!(percentile(&sorted, 1.0), 50.0);
        assert_eq!(percentile(&[7.0], 0.99), 7.0);
    }

    #[test]
    fn p가_1을_넘으면_마지막_값으로_막는다() {
        assert_eq!(percentile(&[1.0, 2.0, 3.0], 2.0), 3.0);
        assert_eq!(percentile(&[1.0, 2.0, 3.0], f64::INFINITY), 3.0);
    }

    #[test]
    fn p가_음수거나_nan이면_값이_없다() {
        assert!(percentile(&[1.0, 2.0, 3.0], -0.5).is_nan());
        assert!(percentile(&[1.0, 2.0, 3.0], f64::NAN).is_nan());
    }
}
