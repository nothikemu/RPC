//! Reconnect backoff with jitter and a client-side activity rate limit.

use std::collections::VecDeque;
use std::time::Duration;

use tokio::time::Instant;

/// Exponential backoff with "equal jitter": each delay is drawn uniformly
/// from `[d/2, d]`, where `d = min(base · 2^attempt, max)`. Jitter avoids
/// synchronized retries; the lower bound guarantees we never spin.
#[derive(Debug)]
pub struct Backoff {
    base: Duration,
    max: Duration,
    attempt: u32,
    rng: fastrand::Rng,
}

impl Backoff {
    pub fn new(base: Duration, max: Duration) -> Self {
        Self::with_rng(base, max, fastrand::Rng::new())
    }

    pub fn with_rng(base: Duration, max: Duration, rng: fastrand::Rng) -> Self {
        Self {
            base,
            max,
            attempt: 0,
            rng,
        }
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// Upper bound of the delay for the current attempt, without jitter.
    pub fn ceiling(&self) -> Duration {
        let factor = 1u32 << self.attempt.min(16);
        self.base.saturating_mul(factor).min(self.max)
    }

    pub fn next_delay(&mut self) -> Duration {
        let ceiling = self.ceiling();
        self.attempt = self.attempt.saturating_add(1);
        let ceiling_ms = u64::try_from(ceiling.as_millis()).unwrap_or(u64::MAX);
        let half = ceiling_ms / 2;
        Duration::from_millis(half + self.rng.u64(0..=ceiling_ms - half))
    }
}

/// Sliding-window limiter: at most `max` sends within any `window`.
///
/// Discord documents no exact `SET_ACTIVITY` rate limit; its legacy SDKs
/// throttled updates and the RPC server closes connections that send too
/// fast (close code 4002). DeskPresence allows 5 updates per 20 seconds and
/// coalesces anything faster into the latest desired activity.
#[derive(Debug)]
pub struct RateLimiter {
    window: Duration,
    max: usize,
    sent: VecDeque<Instant>,
}

impl RateLimiter {
    pub fn new(max: usize, window: Duration) -> Self {
        Self {
            window,
            max: max.max(1),
            sent: VecDeque::new(),
        }
    }

    /// `None` when a send is allowed now, otherwise the earliest allowed time.
    pub fn ready_at(&mut self, now: Instant) -> Option<Instant> {
        while self
            .sent
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= self.window)
        {
            self.sent.pop_front();
        }
        if self.sent.len() < self.max {
            None
        } else {
            self.sent.front().map(|t| *t + self.window)
        }
    }

    pub fn record(&mut self, now: Instant) {
        self.sent.push_back(now);
        while self.sent.len() > self.max {
            self.sent.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_grow_with_jitter_and_cap() {
        let mut b = Backoff::with_rng(
            Duration::from_secs(1),
            Duration::from_secs(60),
            fastrand::Rng::with_seed(7),
        );
        let expected_ceilings = [1, 2, 4, 8, 16, 32, 60, 60, 60];
        for ceiling in expected_ceilings {
            let delay = b.next_delay();
            let ceiling = Duration::from_secs(ceiling);
            assert!(delay <= ceiling, "{delay:?} > {ceiling:?}");
            assert!(delay >= ceiling / 2, "{delay:?} < half of {ceiling:?}");
        }
        assert_eq!(b.attempt(), 9);
        b.reset();
        assert!(b.next_delay() <= Duration::from_secs(1));
    }

    #[test]
    fn jitter_varies() {
        let mut values = std::collections::HashSet::new();
        for seed in 0..20 {
            let mut b = Backoff::with_rng(
                Duration::from_secs(10),
                Duration::from_secs(60),
                fastrand::Rng::with_seed(seed),
            );
            values.insert(b.next_delay());
        }
        assert!(values.len() > 1);
    }

    #[test]
    fn huge_attempt_counts_do_not_overflow() {
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(60));
        for _ in 0..10_000 {
            assert!(b.next_delay() <= Duration::from_secs(60));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limiter_enforces_window() {
        let mut r = RateLimiter::new(5, Duration::from_secs(20));
        let start = Instant::now();
        for _ in 0..5 {
            assert_eq!(r.ready_at(Instant::now()), None);
            r.record(Instant::now());
            tokio::time::advance(Duration::from_secs(1)).await;
        }
        // Sixth send must wait until the first one leaves the window.
        assert_eq!(r.ready_at(Instant::now()), Some(start + Duration::from_secs(20)));
        tokio::time::advance(Duration::from_secs(15)).await;
        assert_eq!(r.ready_at(Instant::now()), None);
    }
}
