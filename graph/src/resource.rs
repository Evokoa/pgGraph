//! Checked resource units and statement-local reservation accounting.
//!
//! PostgreSQL backend work is single-threaded, so the governor uses interior
//! counters instead of synchronization primitives. Memory reservations are
//! represented by leases; dropping a lease returns its allowance. Elapsed-time
//! checks are monotonic for the lifetime of the governor.

use std::cell::Cell;
use std::fmt;
use std::time::{Duration, Instant};

/// Exact byte quantity used for allocation and residency decisions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ByteCount(u64);

impl ByteCount {
    pub(crate) const ZERO: Self = Self(0);

    pub(crate) const fn from_bytes(bytes: u64) -> Self {
        Self(bytes)
    }

    pub(crate) fn from_mib(mib: u64) -> Option<Self> {
        mib.checked_mul(1_048_576).map(Self)
    }

    pub(crate) const fn as_u64(self) -> u64 {
        self.0
    }

    pub(crate) fn as_mib_f64(self) -> f64 {
        self.0 as f64 / 1_048_576.0
    }

    pub(crate) fn ceil_mib(self) -> u64 {
        self.0 / 1_048_576 + u64::from(!self.0.is_multiple_of(1_048_576))
    }

    pub(crate) fn checked_add(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Self)
    }

    pub(crate) fn checked_mul(self, multiplier: u64) -> Option<Self> {
        self.0.checked_mul(multiplier).map(Self)
    }
}

/// Maximum private-memory bytes available to one governed operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MemoryBudget(ByteCount);

impl MemoryBudget {
    pub(crate) const fn new(bytes: ByteCount) -> Self {
        Self(bytes)
    }

    pub(crate) const fn bytes(self) -> ByteCount {
        self.0
    }
}

/// Elapsed-time allowance measured from governor construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ElapsedBudget(Duration);

impl ElapsedBudget {
    pub(crate) const fn new(duration: Duration) -> Self {
        Self(duration)
    }

    pub(crate) const fn duration(self) -> Duration {
        self.0
    }
}

/// Named operation phase used in accounting and stable diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResourcePhase {
    BuildServing,
    BuildReplacement,
}

impl ResourcePhase {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::BuildServing => "build.serving",
            Self::BuildReplacement => "build.replacement",
        }
    }
}

/// Resource family whose configured limit was exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResourceKind {
    Memory,
    Elapsed,
}

impl ResourceKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory bytes",
            Self::Elapsed => "elapsed microseconds",
        }
    }
}

/// Typed failure returned before an operation exceeds a resource limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResourceLimitError {
    kind: ResourceKind,
    phase: ResourcePhase,
    used: u64,
    requested: u64,
    limit: u64,
}

impl ResourceLimitError {
    const fn new(
        kind: ResourceKind,
        phase: ResourcePhase,
        used: u64,
        requested: u64,
        limit: u64,
    ) -> Self {
        Self {
            kind,
            phase,
            used,
            requested,
            limit,
        }
    }

    pub(crate) const fn kind(self) -> ResourceKind {
        self.kind
    }

    pub(crate) const fn used(self) -> u64 {
        self.used
    }

    pub(crate) const fn requested(self) -> u64 {
        self.requested
    }

    pub(crate) const fn limit(self) -> u64 {
        self.limit
    }
}

impl fmt::Display for ResourceLimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} limit exceeded in {} (used {}, requested {}, limit {})",
            self.kind.as_str(),
            self.phase.as_str(),
            self.used,
            self.requested,
            self.limit
        )
    }
}

impl std::error::Error for ResourceLimitError {}

/// Hard limits resolved for one statement or maintenance operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResourceLimits {
    memory: MemoryBudget,
    elapsed: ElapsedBudget,
}

impl ResourceLimits {
    pub(crate) const fn new(memory: MemoryBudget, elapsed: ElapsedBudget) -> Self {
        Self { memory, elapsed }
    }

    pub(crate) const fn memory_only(memory: MemoryBudget) -> Self {
        Self::new(memory, ElapsedBudget::new(Duration::MAX))
    }
}

/// Statement-local resource accountant.
pub(crate) struct ResourceGovernor {
    limits: ResourceLimits,
    started: Instant,
    memory_used: Cell<u64>,
    memory_peak: Cell<u64>,
    memory_peak_phase: Cell<Option<ResourcePhase>>,
}

impl ResourceGovernor {
    pub(crate) fn new(limits: ResourceLimits) -> Self {
        Self {
            limits,
            started: Instant::now(),
            memory_used: Cell::new(0),
            memory_peak: Cell::new(0),
            memory_peak_phase: Cell::new(None),
        }
    }

    pub(crate) fn reserve_memory(
        &self,
        phase: ResourcePhase,
        bytes: ByteCount,
    ) -> Result<ResourceLease<'_>, ResourceLimitError> {
        self.reserve(phase, bytes.as_u64())
    }

    pub(crate) fn check_elapsed(&self, phase: ResourcePhase) -> Result<(), ResourceLimitError> {
        self.check_elapsed_duration(phase, self.started.elapsed())
    }

    fn check_elapsed_duration(
        &self,
        phase: ResourcePhase,
        elapsed: Duration,
    ) -> Result<(), ResourceLimitError> {
        let elapsed = elapsed.as_micros().min(u128::from(u64::MAX)) as u64;
        let limit = self
            .limits
            .elapsed
            .duration()
            .as_micros()
            .min(u128::from(u64::MAX)) as u64;
        if elapsed > limit {
            Err(ResourceLimitError::new(
                ResourceKind::Elapsed,
                phase,
                elapsed,
                0,
                limit,
            ))
        } else {
            Ok(())
        }
    }

    pub(crate) fn memory_peak(&self) -> ByteCount {
        ByteCount::from_bytes(self.memory_peak.get())
    }

    pub(crate) fn memory_peak_phase(&self) -> Option<ResourcePhase> {
        self.memory_peak_phase.get()
    }

    fn reserve(
        &self,
        phase: ResourcePhase,
        requested: u64,
    ) -> Result<ResourceLease<'_>, ResourceLimitError> {
        let current = &self.memory_used;
        let peak = &self.memory_peak;
        let limit = self.limits.memory.bytes().as_u64();
        let used = current.get();
        let Some(next) = used.checked_add(requested) else {
            return Err(ResourceLimitError::new(
                ResourceKind::Memory,
                phase,
                used,
                requested,
                limit,
            ));
        };
        if next > limit {
            return Err(ResourceLimitError::new(
                ResourceKind::Memory,
                phase,
                used,
                requested,
                limit,
            ));
        }
        current.set(next);
        if next > peak.get() {
            peak.set(next);
            self.memory_peak_phase.set(Some(phase));
        }
        Ok(ResourceLease {
            governor: self,
            phase,
            amount: requested,
        })
    }

    fn release_memory(&self, amount: u64) {
        let used = self.memory_used.get();
        debug_assert!(amount <= used, "resource lease released more than reserved");
        self.memory_used.set(used.saturating_sub(amount));
    }
}

/// RAII reservation returned by a memory-budget check.
pub(crate) struct ResourceLease<'a> {
    governor: &'a ResourceGovernor,
    phase: ResourcePhase,
    amount: u64,
}

impl ResourceLease<'_> {
    pub(crate) fn try_grow(&mut self, additional: ByteCount) -> Result<(), ResourceLimitError> {
        let mut extra = self.governor.reserve(self.phase, additional.as_u64())?;
        let combined = self.amount.checked_add(extra.amount).ok_or_else(|| {
            ResourceLimitError::new(
                ResourceKind::Memory,
                self.phase,
                self.amount,
                additional.as_u64(),
                u64::MAX,
            )
        })?;
        self.amount = combined;
        extra.amount = 0;
        Ok(())
    }

    pub(crate) const fn amount(&self) -> ByteCount {
        ByteCount::from_bytes(self.amount)
    }
}

impl Drop for ResourceLease<'_> {
    fn drop(&mut self) {
        self.governor.release_memory(self.amount);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(memory: u64) -> ResourceLimits {
        ResourceLimits::new(
            MemoryBudget::new(ByteCount::from_bytes(memory)),
            ElapsedBudget::new(Duration::from_secs(1)),
        )
    }

    #[test]
    fn checked_byte_units_reject_overflow() {
        assert_eq!(
            ByteCount::from_mib(2).map(ByteCount::as_u64),
            Some(2_097_152)
        );
        assert!(ByteCount::from_mib(u64::MAX).is_none());
        assert!(ByteCount::from_bytes(u64::MAX)
            .checked_add(ByteCount::from_bytes(1))
            .is_none());
        assert_eq!(
            ByteCount::from_bytes(u64::MAX).ceil_mib(),
            17_592_186_044_416
        );
    }

    #[test]
    fn leases_conserve_memory_and_record_peak_phase() {
        let governor = ResourceGovernor::new(limits(100));
        let first = governor
            .reserve_memory(ResourcePhase::BuildServing, ByteCount::from_bytes(40))
            .expect("first reservation should fit");
        {
            let second = governor
                .reserve_memory(ResourcePhase::BuildReplacement, ByteCount::from_bytes(50))
                .expect("second reservation should fit");
            assert_eq!(second.amount().as_u64(), 50);
            assert_eq!(governor.memory_peak().as_u64(), 90);
            assert_eq!(
                governor.memory_peak_phase(),
                Some(ResourcePhase::BuildReplacement)
            );
        }
        drop(first);
        governor
            .reserve_memory(ResourcePhase::BuildReplacement, ByteCount::from_bytes(100))
            .expect("dropped leases should return their reservation");
    }

    #[test]
    fn failed_growth_keeps_existing_reservation() {
        let governor = ResourceGovernor::new(limits(100));
        let mut lease = governor
            .reserve_memory(ResourcePhase::BuildReplacement, ByteCount::from_bytes(80))
            .expect("initial reservation should fit");
        let error = lease
            .try_grow(ByteCount::from_bytes(21))
            .expect_err("growth beyond the limit must fail");
        assert_eq!(error.kind(), ResourceKind::Memory);
        assert_eq!(error.used(), 80);
        assert_eq!(lease.amount().as_u64(), 80);
    }

    #[test]
    fn successful_growth_is_released_with_the_original_lease() {
        let governor = ResourceGovernor::new(limits(100));
        {
            let mut lease = governor
                .reserve_memory(ResourcePhase::BuildReplacement, ByteCount::from_bytes(40))
                .expect("initial reservation should fit");
            lease
                .try_grow(ByteCount::from_bytes(60))
                .expect("growth to the limit should fit");
            assert_eq!(lease.amount().as_u64(), 100);
        }
        governor
            .reserve_memory(ResourcePhase::BuildServing, ByteCount::from_bytes(100))
            .expect("grown lease should release its complete reservation");
    }

    #[test]
    fn elapsed_budget_is_checked_monotonically() {
        let governor = ResourceGovernor::new(ResourceLimits::new(
            MemoryBudget::new(ByteCount::from_bytes(1)),
            ElapsedBudget::new(Duration::ZERO),
        ));
        let error = governor
            .check_elapsed_duration(ResourcePhase::BuildReplacement, Duration::from_micros(1))
            .expect_err("nonzero elapsed time must exceed a zero deadline");
        assert_eq!(error.kind(), ResourceKind::Elapsed);
    }
}
