//! Wait and timeout accounting shared by the Vita display gate and its host tests.
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

const GATE_TIMEOUT_US: u32 = 100_000;
const GATE_TIMEOUTS_TO_DISABLE: u32 = 3;
const SCE_KERNEL_ERROR_WAIT_TIMEOUT: i32 = 0x8002_8005_u32 as i32;

pub(super) struct Timeouts {
    total: AtomicU32,
    consecutive: AtomicU32,
}

impl Timeouts {
    pub(super) const fn new() -> Self {
        Self {
            total: AtomicU32::new(0),
            consecutive: AtomicU32::new(0),
        }
    }

    pub(super) fn total(&self) -> u32 {
        self.total.load(Relaxed)
    }

    /// Record each swap on the swapping thread. Returns true when the gate should turn off.
    pub(super) fn record(&self, timed_out: bool) -> bool {
        if timed_out {
            self.total.fetch_add(1, Relaxed);
            self.consecutive.fetch_add(1, Relaxed) + 1 >= GATE_TIMEOUTS_TO_DISABLE
        } else {
            self.consecutive.store(0, Relaxed);
            false
        }
    }
}

/// Ok(true) means the deadline expired with a callback still pending. Err is a kernel failure.
pub(super) fn wait_for_callbacks(
    mut pending: impl FnMut() -> u32,
    mut elapsed_us: impl FnMut() -> u32,
    mut wait: impl FnMut(u32) -> i32,
) -> Result<bool, i32> {
    loop {
        // The wrapping difference is negative if completion has passed the target.
        // Re-check after every wakeup, including a kernel timeout, before testing the deadline.
        if (pending() as i32) <= 0 {
            return Ok(false);
        }
        let elapsed = elapsed_us();
        if elapsed >= GATE_TIMEOUT_US {
            return Ok(true);
        }
        let result = wait(GATE_TIMEOUT_US - elapsed);
        if result < 0 && result != SCE_KERNEL_ERROR_WAIT_TIMEOUT {
            return Err(result);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swaps_without_a_wait_break_the_timeout_streak() {
        let timeouts = Timeouts::new();
        for _ in 0..3 {
            assert!(!timeouts.record(true));
            assert!(!timeouts.record(false));
        }
        assert_eq!(timeouts.total(), 3);
        assert!(!timeouts.record(true));
        assert!(!timeouts.record(true));
        assert!(timeouts.record(true));
        assert_eq!(timeouts.total(), 6);
    }

    #[test]
    fn semaphore_failure_is_distinct_from_timeout() {
        let error = 0x8002_8100_u32 as i32;
        assert_eq!(wait_for_callbacks(|| 1, || 0, |_| error), Err(error));

        let mut elapsed = [0, GATE_TIMEOUT_US].into_iter();
        assert_eq!(
            wait_for_callbacks(|| 1, || elapsed.next().unwrap(), |left| {
                assert_eq!(left, GATE_TIMEOUT_US);
                SCE_KERNEL_ERROR_WAIT_TIMEOUT
            }),
            Ok(true)
        );
    }

    #[test]
    fn completion_past_target_after_wakeup_exits_without_timeout() {
        let mut pending = [1, u32::MAX].into_iter();
        let mut wait_calls = 0;
        let result = wait_for_callbacks(|| pending.next().unwrap(), || 0, |_| {
            wait_calls += 1;
            SCE_KERNEL_ERROR_WAIT_TIMEOUT
        });
        assert_eq!(result, Ok(false));
        assert_eq!(wait_calls, 1);
    }
}
