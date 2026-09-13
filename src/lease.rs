//! Lease admission rules shared by claim sweeps, heartbeats, and completion.
//!
//! A lease is the tuple `(status = running, claimed_by, lease_expires_at)`.
//! The boundary is exact and identical on every mutation path:
//!
//! * a lease is **live** only while `now < lease_expires_at`;
//! * a lease is **expired** once `lease_expires_at <= now`, and an absent
//!   `lease_expires_at` on a running row is treated as expired.
//!
//! The claim-side expiry sweep requeues or fails rows using the same
//! `lease_expires_at <= now` predicate, so a lease can never be simultaneously
//! "expired enough to reassign" and "live enough to heartbeat or complete".
//!
//! Time is read through [`Clock`] so tests can pin the mutation timestamp
//! exactly at, just before, or just after the boundary without sleeping.

use std::{
    fmt,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Duration, Utc};
use thiserror::Error;

use crate::jobs::JobStatus;

/// Source of the mutation timestamp for lease decisions.
///
/// Lease expiry is persisted as an absolute UTC timestamp that several
/// coordinator replicas compare, so the production clock is wall-clock UTC.
/// Tests inject [`ManualClock`] to make every boundary deterministic.
pub trait Clock: Send + Sync + fmt::Debug {
    fn now(&self) -> DateTime<Utc>;
}

/// Production clock backed by the system UTC clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Deterministic, manually advanced clock for tests and simulations.
#[derive(Debug, Clone)]
pub struct ManualClock {
    now: Arc<Mutex<DateTime<Utc>>>,
}

impl ManualClock {
    pub fn new(start: DateTime<Utc>) -> Self {
        Self {
            now: Arc::new(Mutex::new(start)),
        }
    }

    pub fn set(&self, instant: DateTime<Utc>) {
        *self.now.lock().expect("manual clock mutex poisoned") = instant;
    }

    pub fn advance(&self, by: Duration) {
        let mut now = self.now.lock().expect("manual clock mutex poisoned");
        *now += by;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().expect("manual clock mutex poisoned")
    }
}

/// Explicit, redacted rejection of a lease-bound mutation.
///
/// Messages never include worker identifiers, payloads, or results.
#[derive(Debug, Clone, Copy, Error, PartialEq, Eq)]
pub enum LeaseError {
    #[error("job not found")]
    NotFound,
    #[error("job is not running (status: {})", .0.as_str())]
    NotRunning(JobStatus),
    #[error("job is leased by another worker")]
    HeldByAnotherWorker,
    #[error("worker lease has expired")]
    Expired,
}

impl LeaseError {
    /// Stable machine-readable code for HTTP responses.
    pub fn code(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::NotRunning(_) => "job_not_running",
            Self::HeldByAnotherWorker => "lease_not_held",
            Self::Expired => "lease_expired",
        }
    }
}

/// The subset of job state that lease admission depends on.
#[derive(Debug, Clone, Copy)]
pub struct LeaseState<'a> {
    pub status: JobStatus,
    pub claimed_by: Option<&'a str>,
    pub lease_expires_at: Option<DateTime<Utc>>,
}

/// Returns true when the lease is expired at `now` (`lease_expires_at <= now`).
pub fn is_expired(lease_expires_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    lease_expires_at.is_none_or(|expires_at| expires_at <= now)
}

/// Admits a heartbeat or completion only for the current holder of a live lease.
///
/// Checks are ordered so the most specific, least revealing answer wins:
/// terminal/queued rows are `NotRunning`, a different (or absent) holder is
/// `HeldByAnotherWorker`, and only the current holder learns `Expired`.
pub fn admit_holder(
    state: LeaseState<'_>,
    worker_id: &str,
    now: DateTime<Utc>,
) -> Result<(), LeaseError> {
    if state.status != JobStatus::Running {
        return Err(LeaseError::NotRunning(state.status));
    }
    if state.claimed_by != Some(worker_id) {
        return Err(LeaseError::HeldByAnotherWorker);
    }
    if is_expired(state.lease_expires_at, now) {
        return Err(LeaseError::Expired);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-12T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn running<'a>(holder: &'a str, expires_at: Option<DateTime<Utc>>) -> LeaseState<'a> {
        LeaseState {
            status: JobStatus::Running,
            claimed_by: Some(holder),
            lease_expires_at: expires_at,
        }
    }

    #[test]
    fn renewal_just_before_expiry_is_admitted() {
        let expires = t0() + Duration::seconds(60);
        let state = running("worker-1", Some(expires));
        assert_eq!(
            admit_holder(state, "worker-1", expires - Duration::microseconds(1)),
            Ok(())
        );
        // Repeating the same admission is idempotent for the legitimate holder.
        assert_eq!(admit_holder(state, "worker-1", t0()), Ok(()));
        assert_eq!(admit_holder(state, "worker-1", t0()), Ok(()));
    }

    #[test]
    fn mutation_exactly_at_expiry_is_rejected() {
        let expires = t0() + Duration::seconds(60);
        let state = running("worker-1", Some(expires));
        assert_eq!(
            admit_holder(state, "worker-1", expires),
            Err(LeaseError::Expired)
        );
    }

    #[test]
    fn mutation_after_expiry_is_rejected() {
        let expires = t0() + Duration::seconds(60);
        let state = running("worker-1", Some(expires));
        assert_eq!(
            admit_holder(state, "worker-1", expires + Duration::seconds(1)),
            Err(LeaseError::Expired)
        );
    }

    #[test]
    fn running_row_without_a_lease_is_expired() {
        let state = running("worker-1", None);
        assert_eq!(
            admit_holder(state, "worker-1", t0()),
            Err(LeaseError::Expired)
        );
    }

    #[test]
    fn stale_holder_after_reassignment_is_rejected_even_before_new_expiry() {
        let state = running("worker-2", Some(t0() + Duration::seconds(120)));
        assert_eq!(
            admit_holder(state, "worker-1", t0()),
            Err(LeaseError::HeldByAnotherWorker)
        );
    }

    #[test]
    fn terminal_and_queued_rows_are_not_running() {
        for status in [
            JobStatus::Queued,
            JobStatus::Succeeded,
            JobStatus::Failed,
            JobStatus::Cancelled,
        ] {
            let state = LeaseState {
                status,
                claimed_by: None,
                lease_expires_at: None,
            };
            assert_eq!(
                admit_holder(state, "worker-1", t0()),
                Err(LeaseError::NotRunning(status))
            );
        }
    }

    #[test]
    fn error_messages_are_redacted_and_codes_are_stable() {
        let errors = [
            LeaseError::NotFound,
            LeaseError::NotRunning(JobStatus::Cancelled),
            LeaseError::HeldByAnotherWorker,
            LeaseError::Expired,
        ];
        for error in errors {
            assert!(!error.to_string().contains("worker-"));
        }
        assert_eq!(LeaseError::Expired.code(), "lease_expired");
        assert_eq!(LeaseError::HeldByAnotherWorker.code(), "lease_not_held");
        assert_eq!(
            LeaseError::NotRunning(JobStatus::Queued).code(),
            "job_not_running"
        );
    }

    #[test]
    fn manual_clock_is_deterministic() {
        let clock = ManualClock::new(t0());
        assert_eq!(clock.now(), t0());
        clock.advance(Duration::seconds(5));
        assert_eq!(clock.now(), t0() + Duration::seconds(5));
        clock.set(t0());
        assert_eq!(clock.now(), t0());
    }
}
