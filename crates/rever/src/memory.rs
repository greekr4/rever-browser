//! The development build's memory overlay: what the process holds, sampled every
//! second, so growth shows while the app is being worked on.

use std::collections::VecDeque;

/// One sample a second: the overlay's graph covers the last two minutes.
const KEPT: usize = 120;

/// The memory samples of this run.
#[derive(Default)]
pub struct MemoryWatch {
    samples: VecDeque<u64>,
    peak: u64,
}

impl MemoryWatch {
    pub fn record(&mut self, bytes: u64) {
        if self.samples.len() == KEPT {
            self.samples.pop_front();
        }
        self.samples.push_back(bytes);
        self.peak = self.peak.max(bytes);
    }

    /// The newest sample; `None` before the first one (and always in a release build).
    pub fn latest(&self) -> Option<u64> {
        self.samples.back().copied()
    }

    pub fn peak(&self) -> u64 {
        self.peak
    }

    /// Each kept sample as a share (0..1) of the highest of them, oldest first.
    pub fn bars(&self) -> Vec<f32> {
        let most = self.samples.iter().copied().max().unwrap_or(0).max(1);
        self.samples
            .iter()
            .map(|sample| *sample as f32 / most as f32)
            .collect()
    }
}

/// `1,234MB`: whole megabytes with thousands separators.
pub fn megabytes(bytes: u64) -> String {
    let digits = (bytes / (1024 * 1024)).to_string();
    let mut text = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            text.push(',');
        }
        text.push(digit);
    }
    text + "MB"
}

/// The memory this process holds, as Activity Monitor's "Memory" column counts it.
#[cfg(target_os = "macos")]
pub fn footprint() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage_info_v2>::uninit();
    // SAFETY: the buffer is the struct the V2 flavor fills, and it is read only when
    // the call reports success.
    unsafe {
        let filled =
            libc::proc_pid_rusage(libc::getpid(), libc::RUSAGE_INFO_V2, usage.as_mut_ptr().cast());
        (filled == 0).then(|| usage.assume_init().ri_phys_footprint)
    }
}

#[cfg(not(target_os = "macos"))]
pub fn footprint() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 메가바이트는_천_단위로_끊어_쓴다() {
        assert_eq!(megabytes(0), "0MB");
        assert_eq!(megabytes(999 * 1024 * 1024), "999MB");
        assert_eq!(megabytes(1234 * 1024 * 1024), "1,234MB");
        assert_eq!(megabytes(1_234_567 * 1024 * 1024), "1,234,567MB");
    }

    #[test]
    fn 최고값은_남고_그래프는_최근_것만_가장_큰_값에_맞춰_그린다() {
        let mut watch = MemoryWatch::default();
        assert_eq!(watch.latest(), None);
        watch.record(400);
        for _ in 0..KEPT {
            watch.record(100);
        }
        watch.record(200);
        // 오래된 400은 그래프에서 빠졌지만 최고값으로는 남는다.
        assert_eq!((watch.latest(), watch.peak()), (Some(200), 400));
        let bars = watch.bars();
        assert_eq!(bars.len(), KEPT);
        assert_eq!((bars[0], bars[KEPT - 1]), (0.5, 1.0));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn 이_프로세스의_메모리를_읽는다() {
        assert!(footprint().is_some_and(|bytes| bytes > 1024 * 1024));
    }
}
