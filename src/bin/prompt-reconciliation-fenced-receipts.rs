//! Deterministic lease, fencing, CAS receipt, and duplicate-repair semantics.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

const MAX_IDENTIFIER_BYTES: usize = 256;
const MAX_LEASE_TTL_MS: u64 = 60 * 60 * 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseToken {
    pub owner: String,
    pub fence: u64,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub operation_id: String,
    pub mutation_key: String,
    pub canonical_issue_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptOutcome {
    Recorded,
    AlreadyRecorded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateRepair {
    pub canonical_issue_id: String,
    pub aliases: Vec<String>,
    pub receipt_outcome: ReceiptOutcome,
    pub receipt_generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateError {
    InvalidIdentifier,
    InvalidLeaseTtl,
    LeaseHeld,
    LeaseUnavailable,
    LeaseExpired,
    StaleFence,
    GenerationConflict,
    ReceiptConflict,
    EmptyCandidateSet,
    CounterOverflow,
}

impl fmt::Display for StateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidIdentifier => "identifier is empty, oversized, or contains unsafe bytes",
            Self::InvalidLeaseTtl => "lease TTL is zero or exceeds the configured maximum",
            Self::LeaseHeld => "an unexpired lease is already held",
            Self::LeaseUnavailable => "no active lease exists",
            Self::LeaseExpired => "the lease expired before the requested operation",
            Self::StaleFence => "the supplied lease owner or fencing token is stale",
            Self::GenerationConflict => "receipt compare-and-set generation does not match",
            Self::ReceiptConflict => "an operation receipt already names a different result",
            Self::EmptyCandidateSet => "duplicate repair requires at least one candidate issue",
            Self::CounterOverflow => "a monotonic state counter overflowed",
        })
    }
}

impl std::error::Error for StateError {}

#[derive(Debug, Default)]
pub struct FencedReceiptState {
    next_fence: u64,
    receipt_generation: u64,
    lease: Option<LeaseToken>,
    receipts: BTreeMap<String, Receipt>,
    aliases: BTreeMap<String, String>,
}

impl FencedReceiptState {
    pub fn acquire(
        &mut self,
        owner: &str,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<LeaseToken, StateError> {
        validate_identifier(owner)?;
        validate_ttl(ttl_ms)?;
        if self
            .lease
            .as_ref()
            .is_some_and(|lease| lease.expires_at_ms > now_ms)
        {
            return Err(StateError::LeaseHeld);
        }
        self.next_fence = self
            .next_fence
            .checked_add(1)
            .ok_or(StateError::CounterOverflow)?;
        let expires_at_ms = now_ms
            .checked_add(ttl_ms)
            .ok_or(StateError::CounterOverflow)?;
        let token = LeaseToken {
            owner: owner.to_owned(),
            fence: self.next_fence,
            expires_at_ms,
        };
        self.lease = Some(token.clone());
        Ok(token)
    }

    pub fn renew(
        &mut self,
        token: &LeaseToken,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<LeaseToken, StateError> {
        validate_ttl(ttl_ms)?;
        self.require_active(token, now_ms)?;
        let expires_at_ms = now_ms
            .checked_add(ttl_ms)
            .ok_or(StateError::CounterOverflow)?;
        let renewed = LeaseToken {
            owner: token.owner.clone(),
            fence: token.fence,
            expires_at_ms,
        };
        self.lease = Some(renewed.clone());
        Ok(renewed)
    }

    pub fn release(&mut self, token: &LeaseToken, now_ms: u64) -> Result<(), StateError> {
        self.require_active(token, now_ms)?;
        self.lease = None;
        Ok(())
    }

    pub fn current_receipt_generation(&self) -> u64 {
        self.receipt_generation
    }

    pub fn receipt(&self, operation_id: &str) -> Option<&Receipt> {
        self.receipts.get(operation_id)
    }

    pub fn canonical_issue<'a>(&'a self, issue_id: &'a str) -> &'a str {
        self.aliases.get(issue_id).map_or(issue_id, String::as_str)
    }

    pub fn record(
        &mut self,
        token: &LeaseToken,
        now_ms: u64,
        expected_generation: u64,
        receipt: Receipt,
    ) -> Result<ReceiptOutcome, StateError> {
        self.require_active(token, now_ms)?;
        validate_receipt(&receipt)?;
        if expected_generation != self.receipt_generation {
            return Err(StateError::GenerationConflict);
        }
        if let Some(existing) = self.receipts.get(&receipt.operation_id) {
            if existing.mutation_key == receipt.mutation_key
                && existing.canonical_issue_id == receipt.canonical_issue_id
            {
                return Ok(ReceiptOutcome::AlreadyRecorded);
            }
            return Err(StateError::ReceiptConflict);
        }
        self.receipt_generation = self
            .receipt_generation
            .checked_add(1)
            .ok_or(StateError::CounterOverflow)?;
        self.receipts.insert(receipt.operation_id.clone(), receipt);
        Ok(ReceiptOutcome::Recorded)
    }

    pub fn repair_duplicates(
        &mut self,
        token: &LeaseToken,
        now_ms: u64,
        expected_generation: u64,
        operation_id: &str,
        mutation_key: &str,
        candidates: impl IntoIterator<Item = String>,
    ) -> Result<DuplicateRepair, StateError> {
        self.require_active(token, now_ms)?;
        validate_identifier(operation_id)?;
        validate_identifier(mutation_key)?;
        if expected_generation != self.receipt_generation {
            return Err(StateError::GenerationConflict);
        }
        let candidates = candidates
            .into_iter()
            .map(|candidate| {
                validate_identifier(&candidate)?;
                Ok(candidate)
            })
            .collect::<Result<BTreeSet<_>, StateError>>()?;
        let canonical_issue_id = candidates
            .iter()
            .next()
            .cloned()
            .ok_or(StateError::EmptyCandidateSet)?;
        let aliases = candidates
            .iter()
            .filter(|candidate| *candidate != &canonical_issue_id)
            .cloned()
            .collect::<Vec<_>>();
        let outcome = self.record(
            token,
            now_ms,
            expected_generation,
            Receipt {
                operation_id: operation_id.to_owned(),
                mutation_key: mutation_key.to_owned(),
                canonical_issue_id: canonical_issue_id.clone(),
            },
        )?;
        match outcome {
            ReceiptOutcome::Recorded => {
                for alias in &aliases {
                    self.aliases
                        .insert(alias.clone(), canonical_issue_id.clone());
                }
            }
            ReceiptOutcome::AlreadyRecorded => {
                let consistent = aliases.iter().all(|alias| {
                    self.aliases.get(alias) == Some(&canonical_issue_id)
                });
                if !consistent {
                    return Err(StateError::ReceiptConflict);
                }
            }
        }
        Ok(DuplicateRepair {
            canonical_issue_id,
            aliases,
            receipt_outcome: outcome,
            receipt_generation: self.receipt_generation,
        })
    }

    fn require_active(&self, token: &LeaseToken, now_ms: u64) -> Result<(), StateError> {
        let current = self.lease.as_ref().ok_or(StateError::LeaseUnavailable)?;
        if current.owner != token.owner || current.fence != token.fence {
            return Err(StateError::StaleFence);
        }
        if current.expires_at_ms <= now_ms || token.expires_at_ms <= now_ms {
            return Err(StateError::LeaseExpired);
        }
        Ok(())
    }
}

fn validate_ttl(ttl_ms: u64) -> Result<(), StateError> {
    if ttl_ms == 0 || ttl_ms > MAX_LEASE_TTL_MS {
        Err(StateError::InvalidLeaseTtl)
    } else {
        Ok(())
    }
}

fn validate_identifier(value: &str) -> Result<(), StateError> {
    let valid = !value.is_empty()
        && value.len() <= MAX_IDENTIFIER_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        });
    if valid {
        Ok(())
    } else {
        Err(StateError::InvalidIdentifier)
    }
}

fn validate_receipt(receipt: &Receipt) -> Result<(), StateError> {
    validate_identifier(&receipt.operation_id)?;
    validate_identifier(&receipt.mutation_key)?;
    validate_identifier(&receipt.canonical_issue_id)
}

fn main() {
    eprintln!(
        "prompt-reconciliation-fenced-receipts is a state-machine test target; integrate through prompt-intake"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(issue: &str) -> Receipt {
        Receipt {
            operation_id: "operation-1".to_owned(),
            mutation_key: "mutation-1".to_owned(),
            canonical_issue_id: issue.to_owned(),
        }
    }

    fn acquire(state: &mut FencedReceiptState, owner: &str, now_ms: u64) -> LeaseToken {
        match state.acquire(owner, now_ms, 100) {
            Ok(token) => token,
            Err(error) => panic!("lease acquisition failed: {error}"),
        }
    }

    #[test]
    fn active_lease_blocks_competing_owner() {
        let mut state = FencedReceiptState::default();
        let _token = acquire(&mut state, "worker-a", 100);
        assert_eq!(state.acquire("worker-b", 120, 50), Err(StateError::LeaseHeld));
    }

    #[test]
    fn reacquisition_increments_fence_and_rejects_stale_writer() {
        let mut state = FencedReceiptState::default();
        let first = acquire(&mut state, "worker-a", 100);
        let second = acquire(&mut state, "worker-b", 201);
        assert!(second.fence > first.fence);
        assert_eq!(
            state.record(&first, 202, 0, receipt("DEN-1")),
            Err(StateError::StaleFence)
        );
        assert_eq!(
            state.record(&second, 202, 0, receipt("DEN-1")),
            Ok(ReceiptOutcome::Recorded)
        );
    }

    #[test]
    fn expired_token_cannot_mutate_or_renew() {
        let mut state = FencedReceiptState::default();
        let token = acquire(&mut state, "worker-a", 100);
        assert_eq!(
            state.record(&token, 200, 0, receipt("DEN-1")),
            Err(StateError::LeaseExpired)
        );
        assert_eq!(state.renew(&token, 200, 50), Err(StateError::LeaseExpired));
    }

    #[test]
    fn receipt_compare_and_set_is_monotonic() {
        let mut state = FencedReceiptState::default();
        let token = acquire(&mut state, "worker-a", 100);
        assert_eq!(
            state.record(&token, 101, 0, receipt("DEN-1")),
            Ok(ReceiptOutcome::Recorded)
        );
        assert_eq!(state.current_receipt_generation(), 1);
        assert_eq!(
            state.record(
                &token,
                102,
                0,
                Receipt {
                    operation_id: "operation-2".to_owned(),
                    mutation_key: "mutation-2".to_owned(),
                    canonical_issue_id: "DEN-2".to_owned(),
                }
            ),
            Err(StateError::GenerationConflict)
        );
    }

    #[test]
    fn exact_receipt_rerun_is_a_noop() {
        let mut state = FencedReceiptState::default();
        let token = acquire(&mut state, "worker-a", 100);
        assert_eq!(
            state.record(&token, 101, 0, receipt("DEN-1")),
            Ok(ReceiptOutcome::Recorded)
        );
        assert_eq!(
            state.record(&token, 102, 1, receipt("DEN-1")),
            Ok(ReceiptOutcome::AlreadyRecorded)
        );
        assert_eq!(state.current_receipt_generation(), 1);
    }

    #[test]
    fn conflicting_receipt_fails_closed() {
        let mut state = FencedReceiptState::default();
        let token = acquire(&mut state, "worker-a", 100);
        assert!(state.record(&token, 101, 0, receipt("DEN-1")).is_ok());
        assert_eq!(
            state.record(&token, 102, 1, receipt("DEN-2")),
            Err(StateError::ReceiptConflict)
        );
        assert_eq!(state.receipt("operation-1"), Some(&receipt("DEN-1")));
    }

    #[test]
    fn duplicate_repair_is_deterministic_and_idempotent() {
        let mut state = FencedReceiptState::default();
        let token = acquire(&mut state, "worker-a", 100);
        let repaired = state.repair_duplicates(
            &token,
            101,
            0,
            "repair-operation",
            "duplicate-race",
            ["DEN-30".to_owned(), "DEN-10".to_owned(), "DEN-20".to_owned()],
        );
        assert!(repaired.is_ok());
        let Ok(repaired) = repaired else {
            return;
        };
        assert_eq!(repaired.canonical_issue_id, "DEN-10");
        assert_eq!(repaired.aliases, vec!["DEN-20", "DEN-30"]);
        assert_eq!(state.canonical_issue("DEN-30"), "DEN-10");
        let rerun = state.repair_duplicates(
            &token,
            102,
            1,
            "repair-operation",
            "duplicate-race",
            ["DEN-20".to_owned(), "DEN-10".to_owned(), "DEN-30".to_owned()],
        );
        assert!(matches!(
            rerun,
            Ok(DuplicateRepair {
                receipt_outcome: ReceiptOutcome::AlreadyRecorded,
                receipt_generation: 1,
                ..
            })
        ));
    }

    #[test]
    fn release_requires_current_unexpired_fence() {
        let mut state = FencedReceiptState::default();
        let token = acquire(&mut state, "worker-a", 100);
        let stale = LeaseToken {
            owner: token.owner.clone(),
            fence: token.fence.saturating_add(1),
            expires_at_ms: token.expires_at_ms,
        };
        assert_eq!(state.release(&stale, 101), Err(StateError::StaleFence));
        assert_eq!(state.release(&token, 101), Ok(()));
        assert_eq!(
            state.record(&token, 102, 0, receipt("DEN-1")),
            Err(StateError::LeaseUnavailable)
        );
    }
}
