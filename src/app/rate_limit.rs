//! Per-peer-IP rate limiting for remote auth failures.
//!
//! The remote listener is directly internet-exposed (router port forward, no
//! reverse proxy), so the peer socket address is the real client address and
//! `X-Forwarded-For` must never be consulted.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Failures allowed before lockouts begin.
const FREE_FAILURES: u32 = 5;
/// First lockout duration; doubles per additional failure.
const BASE_LOCKOUT: Duration = Duration::from_secs(30);
/// Maximum lockout duration.
const MAX_LOCKOUT: Duration = Duration::from_secs(60 * 60);
/// Entries idle longer than this are pruned.
const STALE_AFTER: Duration = Duration::from_secs(2 * 60 * 60);
/// Prune at most once per this interval to keep the hot path cheap.
const PRUNE_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug)]
struct PeerFailures {
    failures: u32,
    locked_until: Option<Instant>,
    last_seen: Instant,
}

#[derive(Debug)]
pub struct AuthRateLimiter {
    peers: Mutex<HashMap<IpAddr, PeerFailures>>,
    last_prune: Mutex<Instant>,
}

impl Default for AuthRateLimiter {
    fn default() -> Self {
        Self {
            peers: Mutex::new(HashMap::new()),
            last_prune: Mutex::new(Instant::now()),
        }
    }
}

impl AuthRateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Remaining lockout for a peer, if any. `None` means the request may
    /// proceed to auth.
    pub fn lockout_remaining(&self, peer: IpAddr) -> Option<Duration> {
        self.lockout_remaining_at(peer, Instant::now())
    }

    fn lockout_remaining_at(&self, peer: IpAddr, now: Instant) -> Option<Duration> {
        let peers = self.peers.lock().unwrap();
        peers
            .get(&peer)
            .and_then(|entry| entry.locked_until)
            .and_then(|until| until.checked_duration_since(now))
            .filter(|remaining| !remaining.is_zero())
    }

    /// Record an auth failure and return the lockout it triggered, if any.
    pub fn record_failure(&self, peer: IpAddr) -> Option<Duration> {
        self.record_failure_at(peer, Instant::now())
    }

    fn record_failure_at(&self, peer: IpAddr, now: Instant) -> Option<Duration> {
        self.maybe_prune(now);
        let mut peers = self.peers.lock().unwrap();
        let entry = peers.entry(peer).or_insert(PeerFailures {
            failures: 0,
            locked_until: None,
            last_seen: now,
        });
        entry.failures = entry.failures.saturating_add(1);
        entry.last_seen = now;
        if entry.failures <= FREE_FAILURES {
            return None;
        }
        let exponent = (entry.failures - FREE_FAILURES - 1).min(31);
        let lockout = BASE_LOCKOUT
            .checked_mul(1_u32 << exponent)
            .map_or(MAX_LOCKOUT, |lockout| lockout.min(MAX_LOCKOUT));
        entry.locked_until = Some(now + lockout);
        Some(lockout)
    }

    /// Clear failure history after a successful authentication.
    pub fn record_success(&self, peer: IpAddr) {
        self.peers.lock().unwrap().remove(&peer);
    }

    fn maybe_prune(&self, now: Instant) {
        {
            let mut last_prune = self.last_prune.lock().unwrap();
            if now.duration_since(*last_prune) < PRUNE_INTERVAL {
                return;
            }
            *last_prune = now;
        }
        let mut peers = self.peers.lock().unwrap();
        peers.retain(|_, entry| {
            now.duration_since(entry.last_seen) < STALE_AFTER
                || entry.locked_until.is_some_and(|until| until > now)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer() -> IpAddr {
        "203.0.113.7".parse().unwrap()
    }

    #[test]
    fn free_failures_do_not_lock_out() {
        let limiter = AuthRateLimiter::new();
        let now = Instant::now();

        for _ in 0..FREE_FAILURES {
            assert!(limiter.record_failure_at(peer(), now).is_none());
        }
        assert!(limiter.lockout_remaining_at(peer(), now).is_none());
    }

    #[test]
    fn lockout_grows_exponentially_and_caps_at_one_hour() {
        let limiter = AuthRateLimiter::new();
        let now = Instant::now();

        for _ in 0..FREE_FAILURES {
            limiter.record_failure_at(peer(), now);
        }
        assert_eq!(limiter.record_failure_at(peer(), now), Some(BASE_LOCKOUT));
        assert_eq!(
            limiter.record_failure_at(peer(), now),
            Some(BASE_LOCKOUT * 2)
        );
        assert_eq!(
            limiter.record_failure_at(peer(), now),
            Some(BASE_LOCKOUT * 4)
        );
        for _ in 0..40 {
            limiter.record_failure_at(peer(), now);
        }
        assert_eq!(limiter.record_failure_at(peer(), now), Some(MAX_LOCKOUT));
        assert!(limiter.lockout_remaining_at(peer(), now).is_some());
    }

    #[test]
    fn lockout_expires_after_its_duration() {
        let limiter = AuthRateLimiter::new();
        let now = Instant::now();

        for _ in 0..=FREE_FAILURES {
            limiter.record_failure_at(peer(), now);
        }

        assert!(limiter.lockout_remaining_at(peer(), now).is_some());
        assert!(
            limiter
                .lockout_remaining_at(peer(), now + BASE_LOCKOUT)
                .is_none()
        );
    }

    #[test]
    fn success_resets_failure_history() {
        let limiter = AuthRateLimiter::new();
        let now = Instant::now();

        for _ in 0..=FREE_FAILURES {
            limiter.record_failure_at(peer(), now);
        }
        limiter.record_success(peer());

        assert!(limiter.lockout_remaining_at(peer(), now).is_none());
        assert!(limiter.record_failure_at(peer(), now).is_none());
    }

    #[test]
    fn independent_peers_are_limited_independently() {
        let limiter = AuthRateLimiter::new();
        let now = Instant::now();
        let other: IpAddr = "198.51.100.9".parse().unwrap();

        for _ in 0..=FREE_FAILURES {
            limiter.record_failure_at(peer(), now);
        }

        assert!(limiter.lockout_remaining_at(peer(), now).is_some());
        assert!(limiter.lockout_remaining_at(other, now).is_none());
    }

    #[test]
    fn stale_entries_are_pruned() {
        let limiter = AuthRateLimiter::new();
        let now = Instant::now();

        limiter.record_failure_at(peer(), now);
        // A later failure from another peer triggers pruning of stale entries.
        let later = now + STALE_AFTER + PRUNE_INTERVAL;
        let other: IpAddr = "198.51.100.9".parse().unwrap();
        limiter.record_failure_at(other, later);

        let peers = limiter.peers.lock().unwrap();
        assert!(!peers.contains_key(&peer()));
        assert!(peers.contains_key(&other));
    }
}
