//! Checked resource units and statement-local reservation accounting.
//!
//! PostgreSQL backend work is single-threaded, so the governor uses interior
//! counters instead of synchronization primitives. Memory reservations are
//! represented by leases; dropping a lease returns its allowance. Elapsed-time
//! checks are monotonic for the lifetime of the governor.

use std::cell::Cell;
use std::fmt;
use std::time::{Duration, Instant};

/// Yield to PostgreSQL cancellation and statement-timeout handling.
#[inline]
pub(crate) fn check_postgres_interrupts() {
    #[cfg(not(test))]
    pgrx::check_for_interrupts!();
}

/// Exact byte quantity used for allocation and residency decisions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ByteCount(u64);

impl ByteCount {
    pub(crate) const ZERO: Self = Self(0);

    pub(crate) const fn from_bytes(bytes: u64) -> Self {
        Self(bytes)
    }

    pub(crate) fn from_usize(bytes: usize) -> Option<Self> {
        u64::try_from(bytes).ok().map(Self)
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

    pub(crate) fn checked_sub(self, other: Self) -> Option<Self> {
        self.0.checked_sub(other.0).map(Self)
    }

    pub(crate) fn checked_mul(self, multiplier: u64) -> Option<Self> {
        self.0.checked_mul(multiplier).map(Self)
    }
}

/// Resolve the byte target for one transient build batch.
pub(crate) fn adaptive_build_batch_target(available: ByteCount) -> ByteCount {
    const MIN_BATCH_BYTES: u64 = 64 * 1024;
    const MAX_BATCH_BYTES: u64 = 8 * 1024 * 1024;

    let proportional = available.as_u64() / 16;
    ByteCount::from_bytes(
        proportional
            .clamp(MIN_BATCH_BYTES, MAX_BATCH_BYTES)
            .min(available.as_u64()),
    )
}

/// Resolve a query governor after accounting for backend-private graph residency.
pub(crate) fn query_governor(resident: ByteCount) -> ResourceGovernor {
    operation_governor(
        "query",
        resident,
        configured_query_memory_mb(),
        RowCount::UNLIMITED,
        WorkUnits::new(configured_query_work_limit()),
    )
}

/// Resolve the private-memory budget for loading one persisted graph artifact.
pub(crate) fn load_governor(resident: ByteCount) -> ResourceGovernor {
    operation_governor(
        "load",
        resident,
        configured_memory_limit_mb(),
        RowCount::UNLIMITED,
        WorkUnits::UNLIMITED,
    )
}

/// Resolve a maintenance governor after accounting for backend-private graph residency.
pub(crate) fn maintenance_governor(resident: ByteCount, rows: RowCount) -> ResourceGovernor {
    operation_governor(
        "maintenance",
        resident,
        configured_maintenance_memory_mb(),
        rows,
        WorkUnits::UNLIMITED,
    )
}

/// Resolve a whole-graph analytics governor with maintenance memory and query work limits.
pub(crate) fn analytics_governor(resident: ByteCount) -> ResourceGovernor {
    operation_governor(
        "analytics",
        resident,
        configured_maintenance_memory_mb(),
        RowCount::UNLIMITED,
        WorkUnits::new(configured_query_work_limit()),
    )
}

fn operation_governor(
    operation: &'static str,
    resident: ByteCount,
    workspace_mb: i32,
    rows: RowCount,
    work: WorkUnits,
) -> ResourceGovernor {
    let hard =
        ByteCount::from_mib(configured_memory_limit_mb().max(1) as u64).unwrap_or(ByteCount::ZERO);
    let remaining = hard.checked_sub(resident).unwrap_or(ByteCount::ZERO);
    let workspace = ByteCount::from_mib(workspace_mb.max(1) as u64).unwrap_or(ByteCount::ZERO);
    let memory = ByteCount::from_bytes(remaining.as_u64().min(workspace.as_u64()));
    let disk = ByteCount::from_mib(configured_spill_disk_limit_mb().max(1) as u64)
        .unwrap_or(ByteCount::ZERO);
    let timeout_ms = configured_operation_timeout_ms();
    let elapsed = if timeout_ms == 0 {
        Duration::MAX
    } else {
        Duration::from_millis(timeout_ms)
    };
    ResourceGovernor::new_named(
        operation,
        ResourceLimits::bounded(
            MemoryBudget::new(memory),
            DiskBudget::new(disk),
            rows,
            work,
            ElapsedBudget::new(elapsed),
        ),
    )
}

#[cfg(not(test))]
pub(crate) fn configured_memory_limit_mb() -> i32 {
    crate::config::MEMORY_LIMIT_MB.get()
}

#[cfg(test)]
pub(crate) const fn configured_memory_limit_mb() -> i32 {
    2_048
}

#[cfg(not(test))]
fn configured_query_memory_mb() -> i32 {
    crate::config::QUERY_MEMORY_MB.get()
}

#[cfg(test)]
const fn configured_query_memory_mb() -> i32 {
    64
}

#[cfg(not(test))]
fn configured_maintenance_memory_mb() -> i32 {
    crate::config::MAINTENANCE_MEMORY_MB.get()
}

#[cfg(test)]
const fn configured_maintenance_memory_mb() -> i32 {
    256
}

#[cfg(not(test))]
fn configured_spill_disk_limit_mb() -> i32 {
    crate::config::SPILL_DISK_LIMIT_MB.get()
}

#[cfg(test)]
const fn configured_spill_disk_limit_mb() -> i32 {
    4_096
}

#[cfg(not(test))]
fn configured_query_work_limit() -> u64 {
    crate::config::QUERY_WORK_LIMIT.get().max(1) as u64
}

#[cfg(test)]
const fn configured_query_work_limit() -> u64 {
    10_000_000
}

#[cfg(not(test))]
fn configured_operation_timeout_ms() -> u64 {
    crate::config::OPERATION_TIMEOUT_MS.get().max(0) as u64
}

#[cfg(test)]
const fn configured_operation_timeout_ms() -> u64 {
    0
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

/// Maximum spill or temporary artifact bytes available to one operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DiskBudget(ByteCount);

impl DiskBudget {
    pub(crate) const UNLIMITED: Self = Self(ByteCount::from_bytes(u64::MAX));

    pub(crate) const fn new(bytes: ByteCount) -> Self {
        Self(bytes)
    }

    pub(crate) const fn bytes(self) -> ByteCount {
        self.0
    }
}

/// Checked row quantity used by input and output breakers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RowCount(u64);

impl RowCount {
    pub(crate) const UNLIMITED: Self = Self(u64::MAX);

    pub(crate) const fn new(rows: u64) -> Self {
        Self(rows)
    }

    pub(crate) const fn as_u64(self) -> u64 {
        self.0
    }
}

/// Checked abstract work quantity used for expansions and blocking operators.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct WorkUnits(u64);

impl WorkUnits {
    pub(crate) const UNLIMITED: Self = Self(u64::MAX);

    pub(crate) const fn new(units: u64) -> Self {
        Self(units)
    }

    pub(crate) const fn as_u64(self) -> u64 {
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
    Serving,
    SafetyReserve,
    Replacement,
    Nodes,
    NodeBatch,
    Filters,
    Tenants,
    Resolution,
    EdgeBatch,
    EdgeResolve,
    Csr,
    #[allow(dead_code, reason = "used by the R3B run pipeline integrated in R3D")]
    BuildRunWrite,
    #[allow(dead_code, reason = "used by the R3B run pipeline integrated in R3D")]
    BuildRunMerge,
    Persistence,
    LoadMetadata,
    QueryCandidates,
    QueryExpand,
    QueryFrontier,
    QueryPaths,
    QueryHydrate,
    QueryBlocking,
    SyncIngest,
    CompactionMerge,
    AnalyticsWorkspace,
}

impl ResourcePhase {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Serving => "build.serving",
            Self::SafetyReserve => "build.safety_reserve",
            Self::Replacement => "build.replacement",
            Self::Nodes => "build.nodes",
            Self::NodeBatch => "build.node_batch",
            Self::Filters => "build.filters",
            Self::Tenants => "build.tenants",
            Self::Resolution => "build.resolution",
            Self::EdgeBatch => "build.edge_batch",
            Self::EdgeResolve => "build.edge_resolve",
            Self::Csr => "build.csr",
            Self::BuildRunWrite => "build.run_write",
            Self::BuildRunMerge => "build.run_merge",
            Self::Persistence => "build.persistence",
            Self::LoadMetadata => "load.metadata",
            Self::QueryCandidates => "query.candidates",
            Self::QueryExpand => "query.expand",
            Self::QueryFrontier => "query.frontier",
            Self::QueryPaths => "query.paths",
            Self::QueryHydrate => "query.hydrate",
            Self::QueryBlocking => "query.blocking",
            Self::SyncIngest => "sync.ingest",
            Self::CompactionMerge => "compact.merge",
            Self::AnalyticsWorkspace => "analytics.workspace",
        }
    }
}

/// Decision returned before appending a row to an adaptive build batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BatchAction {
    Append,
    Flush,
}

/// Row-and-byte batch policy used by PostgreSQL build spools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AdaptiveBatch {
    phase: ResourcePhase,
    row_limit: usize,
    byte_limit: ByteCount,
    rows: usize,
    bytes: ByteCount,
    pressure_events: u64,
}

impl AdaptiveBatch {
    pub(crate) fn new(phase: ResourcePhase, row_limit: usize, byte_limit: ByteCount) -> Self {
        Self {
            phase,
            row_limit: row_limit.max(1),
            byte_limit,
            rows: 0,
            bytes: ByteCount::ZERO,
            pressure_events: 0,
        }
    }

    pub(crate) fn before_push(&mut self, row_bytes: ByteCount) -> BatchAction {
        let exceeds_rows = self.rows >= self.row_limit;
        let exceeds_bytes = self
            .bytes
            .checked_add(row_bytes)
            .is_none_or(|next| next > self.byte_limit);
        if self.rows != 0 && (exceeds_rows || exceeds_bytes) {
            self.pressure_events = self.pressure_events.saturating_add(1);
            BatchAction::Flush
        } else {
            BatchAction::Append
        }
    }

    pub(crate) fn record(&mut self, row_bytes: ByteCount) -> Result<(), ResourceLimitError> {
        self.bytes = self.bytes.checked_add(row_bytes).ok_or_else(|| {
            ResourceLimitError::new(
                ResourceKind::Memory,
                self.phase,
                self.bytes.as_u64(),
                row_bytes.as_u64(),
                u64::MAX,
            )
        })?;
        self.rows = self.rows.checked_add(1).ok_or_else(|| {
            ResourceLimitError::new(
                ResourceKind::Memory,
                self.phase,
                self.rows as u64,
                1,
                usize::MAX as u64,
            )
        })?;
        Ok(())
    }

    pub(crate) fn is_full(self) -> bool {
        self.rows >= self.row_limit || (self.rows != 0 && self.bytes >= self.byte_limit)
    }

    pub(crate) fn clear(&mut self) {
        self.rows = 0;
        self.bytes = ByteCount::ZERO;
    }

    pub(crate) const fn pressure_events(self) -> u64 {
        self.pressure_events
    }
}

/// Resource family whose configured limit was exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResourceKind {
    Memory,
    Disk,
    Rows,
    Work,
    Elapsed,
}

impl ResourceKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory bytes",
            Self::Disk => "disk bytes",
            Self::Rows => "rows",
            Self::Work => "work units",
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

    pub(crate) const fn phase(self) -> ResourcePhase {
        self.phase
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
    disk: DiskBudget,
    rows: RowCount,
    work: WorkUnits,
    elapsed: ElapsedBudget,
}

impl ResourceLimits {
    pub(crate) const fn new(
        memory: MemoryBudget,
        disk: DiskBudget,
        rows: RowCount,
        work: WorkUnits,
        elapsed: ElapsedBudget,
    ) -> Self {
        Self {
            memory,
            disk,
            rows,
            work,
            elapsed,
        }
    }

    pub(crate) const fn memory_only(memory: MemoryBudget) -> Self {
        Self::new(
            memory,
            DiskBudget::UNLIMITED,
            RowCount::UNLIMITED,
            WorkUnits::UNLIMITED,
            ElapsedBudget::new(Duration::MAX),
        )
    }

    pub(crate) const fn bounded(
        memory: MemoryBudget,
        disk: DiskBudget,
        rows: RowCount,
        work: WorkUnits,
        elapsed: ElapsedBudget,
    ) -> Self {
        Self::new(memory, disk, rows, work, elapsed)
    }
}

/// Statement-local resource accountant.
pub(crate) struct ResourceGovernor {
    operation: Option<&'static str>,
    limits: ResourceLimits,
    started: Instant,
    memory_used: Cell<u64>,
    memory_peak: Cell<u64>,
    memory_peak_phase: Cell<Option<ResourcePhase>>,
    disk_used: Cell<u64>,
    disk_peak: Cell<u64>,
    rows_used: Cell<u64>,
    work_used: Cell<u64>,
}

impl ResourceGovernor {
    pub(crate) fn new(limits: ResourceLimits) -> Self {
        Self {
            operation: None,
            limits,
            started: Instant::now(),
            memory_used: Cell::new(0),
            memory_peak: Cell::new(0),
            memory_peak_phase: Cell::new(None),
            disk_used: Cell::new(0),
            disk_peak: Cell::new(0),
            rows_used: Cell::new(0),
            work_used: Cell::new(0),
        }
    }

    fn new_named(operation: &'static str, limits: ResourceLimits) -> Self {
        let mut governor = Self::new(limits);
        governor.operation = Some(operation);
        governor
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

    pub(crate) fn consume_rows(
        &self,
        phase: ResourcePhase,
        rows: RowCount,
    ) -> Result<(), ResourceLimitError> {
        consume_counter(
            &self.rows_used,
            rows.as_u64(),
            self.limits.rows.as_u64(),
            ResourceKind::Rows,
            phase,
        )
    }

    pub(crate) fn consume_work(
        &self,
        phase: ResourcePhase,
        units: WorkUnits,
    ) -> Result<(), ResourceLimitError> {
        consume_counter(
            &self.work_used,
            units.as_u64(),
            self.limits.work.as_u64(),
            ResourceKind::Work,
            phase,
        )
    }

    pub(crate) fn reserve_disk(
        &self,
        phase: ResourcePhase,
        bytes: ByteCount,
    ) -> Result<DiskLease<'_>, ResourceLimitError> {
        let used = self.disk_used.get();
        let requested = bytes.as_u64();
        let limit = self.limits.disk.bytes().as_u64();
        let next = used.checked_add(requested).ok_or_else(|| {
            ResourceLimitError::new(ResourceKind::Disk, phase, used, requested, limit)
        })?;
        if next > limit {
            return Err(ResourceLimitError::new(
                ResourceKind::Disk,
                phase,
                used,
                requested,
                limit,
            ));
        }
        self.disk_used.set(next);
        self.disk_peak.set(self.disk_peak.get().max(next));
        Ok(DiskLease {
            governor: self,
            amount: requested,
        })
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

    pub(crate) fn memory_used(&self) -> ByteCount {
        ByteCount::from_bytes(self.memory_used.get())
    }

    pub(crate) fn memory_limit(&self) -> ByteCount {
        self.limits.memory.bytes()
    }

    pub(crate) fn memory_peak_phase(&self) -> Option<ResourcePhase> {
        self.memory_peak_phase.get()
    }

    pub(crate) fn disk_peak(&self) -> ByteCount {
        ByteCount::from_bytes(self.disk_peak.get())
    }

    pub(crate) fn rows_used(&self) -> RowCount {
        RowCount::new(self.rows_used.get())
    }

    pub(crate) fn work_used(&self) -> WorkUnits {
        WorkUnits::new(self.work_used.get())
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

    fn release_disk(&self, amount: u64) {
        let used = self.disk_used.get();
        debug_assert!(amount <= used, "disk lease released more than reserved");
        self.disk_used.set(used.saturating_sub(amount));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResourceSnapshot {
    pub(crate) operation: String,
    pub(crate) memory_budget_bytes: u64,
    pub(crate) memory_peak_bytes: u64,
    pub(crate) memory_peak_phase: Option<String>,
    pub(crate) disk_peak_bytes: u64,
    pub(crate) rows: u64,
    pub(crate) work_units: u64,
}

std::thread_local! {
    static LAST_OPERATION: std::cell::RefCell<Option<ResourceSnapshot>> = const { std::cell::RefCell::new(None) };
}

pub(crate) fn last_operation_snapshot() -> Option<ResourceSnapshot> {
    LAST_OPERATION.with(|snapshot| snapshot.borrow().clone())
}

impl Drop for ResourceGovernor {
    fn drop(&mut self) {
        let Some(operation) = self.operation else {
            return;
        };
        let snapshot = ResourceSnapshot {
            operation: operation.to_string(),
            memory_budget_bytes: self.memory_limit().as_u64(),
            memory_peak_bytes: self.memory_peak().as_u64(),
            memory_peak_phase: self
                .memory_peak_phase()
                .map(ResourcePhase::as_str)
                .map(ToString::to_string),
            disk_peak_bytes: self.disk_peak().as_u64(),
            rows: self.rows_used().as_u64(),
            work_units: self.work_used().as_u64(),
        };
        LAST_OPERATION.with(|last| *last.borrow_mut() = Some(snapshot));
    }
}

fn consume_counter(
    current: &Cell<u64>,
    requested: u64,
    limit: u64,
    kind: ResourceKind,
    phase: ResourcePhase,
) -> Result<(), ResourceLimitError> {
    let used = current.get();
    let next = used
        .checked_add(requested)
        .ok_or_else(|| ResourceLimitError::new(kind, phase, used, requested, limit))?;
    if next > limit {
        return Err(ResourceLimitError::new(kind, phase, used, requested, limit));
    }
    current.set(next);
    Ok(())
}

/// RAII reservation for operation-owned spill or staging bytes.
pub(crate) struct DiskLease<'a> {
    governor: &'a ResourceGovernor,
    amount: u64,
}

#[allow(
    dead_code,
    reason = "disk lease resizing supports the R3B run pipeline integrated in R3D"
)]
impl DiskLease<'_> {
    pub(crate) fn try_grow(
        &mut self,
        phase: ResourcePhase,
        additional: ByteCount,
    ) -> Result<(), ResourceLimitError> {
        let mut extra = self.governor.reserve_disk(phase, additional)?;
        self.amount = self.amount.checked_add(extra.amount).ok_or_else(|| {
            ResourceLimitError::new(
                ResourceKind::Disk,
                phase,
                self.amount,
                additional.as_u64(),
                u64::MAX,
            )
        })?;
        extra.amount = 0;
        Ok(())
    }

    pub(crate) const fn amount(&self) -> ByteCount {
        ByteCount::from_bytes(self.amount)
    }

    pub(crate) fn release_all(&mut self) {
        self.governor.release_disk(self.amount);
        self.amount = 0;
    }
}

impl Drop for DiskLease<'_> {
    fn drop(&mut self) {
        self.governor.release_disk(self.amount);
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
        self.try_grow_in(self.phase, additional)
    }

    pub(crate) fn try_grow_in(
        &mut self,
        phase: ResourcePhase,
        additional: ByteCount,
    ) -> Result<(), ResourceLimitError> {
        let mut extra = self.governor.reserve(phase, additional.as_u64())?;
        let combined = self.amount.checked_add(extra.amount).ok_or_else(|| {
            ResourceLimitError::new(
                ResourceKind::Memory,
                phase,
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

    /// Resize this lease without leaving live allocations unaccounted.
    pub(crate) fn try_resize(&mut self, target: ByteCount) -> Result<(), ResourceLimitError> {
        match target.as_u64().cmp(&self.amount) {
            std::cmp::Ordering::Greater => self.try_grow(ByteCount::from_bytes(
                target.as_u64().saturating_sub(self.amount),
            )),
            std::cmp::Ordering::Less => {
                let released = self.amount.saturating_sub(target.as_u64());
                self.governor.release_memory(released);
                self.amount = target.as_u64();
                Ok(())
            }
            std::cmp::Ordering::Equal => Ok(()),
        }
    }

    pub(crate) fn release_all(&mut self) {
        self.governor.release_memory(self.amount);
        self.amount = 0;
    }

    /// Keep this reservation charged until the owning governor is dropped.
    ///
    /// This is used when a phase returns an allocation that remains live in a
    /// later phase of the same governed operation.
    pub(crate) fn retain_until_governor_drop(mut self) {
        self.amount = 0;
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
            DiskBudget::UNLIMITED,
            RowCount::UNLIMITED,
            WorkUnits::UNLIMITED,
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
            .reserve_memory(ResourcePhase::Serving, ByteCount::from_bytes(40))
            .expect("first reservation should fit");
        {
            let second = governor
                .reserve_memory(ResourcePhase::Replacement, ByteCount::from_bytes(50))
                .expect("second reservation should fit");
            assert_eq!(second.amount().as_u64(), 50);
            assert_eq!(governor.memory_peak().as_u64(), 90);
            assert_eq!(
                governor.memory_peak_phase(),
                Some(ResourcePhase::Replacement)
            );
        }
        drop(first);
        governor
            .reserve_memory(ResourcePhase::Replacement, ByteCount::from_bytes(100))
            .expect("dropped leases should return their reservation");
    }

    #[test]
    fn failed_growth_keeps_existing_reservation() {
        let governor = ResourceGovernor::new(limits(100));
        let mut lease = governor
            .reserve_memory(ResourcePhase::Replacement, ByteCount::from_bytes(80))
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
                .reserve_memory(ResourcePhase::Replacement, ByteCount::from_bytes(40))
                .expect("initial reservation should fit");
            lease
                .try_grow(ByteCount::from_bytes(60))
                .expect("growth to the limit should fit");
            assert_eq!(lease.amount().as_u64(), 100);
        }
        governor
            .reserve_memory(ResourcePhase::Serving, ByteCount::from_bytes(100))
            .expect("grown lease should release its complete reservation");
    }

    #[test]
    fn release_all_returns_workspace_without_dropping_the_lease() {
        let governor = ResourceGovernor::new(limits(100));
        let mut workspace = governor
            .reserve_memory(ResourcePhase::NodeBatch, ByteCount::from_bytes(60))
            .expect("workspace reservation should fit");
        workspace.release_all();
        assert_eq!(governor.memory_used(), ByteCount::ZERO);
        workspace
            .try_grow(ByteCount::from_bytes(100))
            .expect("released workspace lease should be reusable");
    }

    #[test]
    fn resize_preserves_accounting_while_growing_and_shrinking() {
        let governor = ResourceGovernor::new(limits(100));
        let mut workspace = governor
            .reserve_memory(ResourcePhase::SyncIngest, ByteCount::from_bytes(40))
            .expect("initial reservation");

        workspace
            .try_resize(ByteCount::from_bytes(75))
            .expect("grow reservation");
        assert_eq!(workspace.amount(), ByteCount::from_bytes(75));
        assert_eq!(governor.memory_used(), ByteCount::from_bytes(75));

        workspace
            .try_resize(ByteCount::from_bytes(25))
            .expect("shrink reservation");
        assert_eq!(workspace.amount(), ByteCount::from_bytes(25));
        assert_eq!(governor.memory_used(), ByteCount::from_bytes(25));
        assert_eq!(governor.memory_peak(), ByteCount::from_bytes(75));
    }

    #[test]
    fn retained_lease_reduces_later_phase_headroom() {
        let governor = ResourceGovernor::new(limits(100));
        governor
            .reserve_memory(ResourcePhase::QueryCandidates, ByteCount::from_bytes(40))
            .expect("reservation")
            .retain_until_governor_drop();

        assert_eq!(governor.memory_used(), ByteCount::from_bytes(40));
        let error =
            match governor.reserve_memory(ResourcePhase::QueryHydrate, ByteCount::from_bytes(61)) {
                Ok(_) => panic!("retained phase memory must reduce later headroom"),
                Err(error) => error,
            };
        assert_eq!(error.used(), 40);
    }

    #[test]
    fn lease_growth_records_the_phase_that_crosses_the_peak() {
        let governor = ResourceGovernor::new(limits(100));
        let mut lease = governor
            .reserve_memory(ResourcePhase::Nodes, ByteCount::from_bytes(40))
            .expect("initial node reservation should fit");
        lease
            .try_grow_in(ResourcePhase::Filters, ByteCount::from_bytes(30))
            .expect("filter growth should fit");

        assert_eq!(governor.memory_peak().as_u64(), 70);
        assert_eq!(governor.memory_peak_phase(), Some(ResourcePhase::Filters));
    }

    #[test]
    fn adaptive_batch_flushes_on_bytes_or_rows_and_accepts_one_large_row() {
        let mut batch = AdaptiveBatch::new(ResourcePhase::NodeBatch, 3, ByteCount::from_bytes(10));
        assert_eq!(
            batch.before_push(ByteCount::from_bytes(6)),
            BatchAction::Append
        );
        batch
            .record(ByteCount::from_bytes(6))
            .expect("batch byte accounting should fit");
        assert_eq!(
            batch.before_push(ByteCount::from_bytes(5)),
            BatchAction::Flush
        );
        batch.clear();

        assert_eq!(
            batch.before_push(ByteCount::from_bytes(20)),
            BatchAction::Append
        );
        batch
            .record(ByteCount::from_bytes(20))
            .expect("one large row should still fit u64 accounting");
        assert!(batch.is_full());
        assert_eq!(batch.pressure_events(), 1);
    }

    #[test]
    fn adaptive_batch_target_is_bounded_by_available_memory() {
        assert_eq!(
            adaptive_build_batch_target(ByteCount::from_bytes(32 * 1024)).as_u64(),
            32 * 1024
        );
        assert_eq!(
            adaptive_build_batch_target(ByteCount::from_mib(64).expect("test MiB should fit"))
                .as_u64(),
            4 * 1024 * 1024
        );
        assert_eq!(
            adaptive_build_batch_target(ByteCount::from_mib(1024).expect("test MiB should fit"))
                .as_u64(),
            8 * 1024 * 1024
        );
    }

    #[test]
    fn elapsed_budget_is_checked_monotonically() {
        let governor = ResourceGovernor::new(ResourceLimits::new(
            MemoryBudget::new(ByteCount::from_bytes(1)),
            DiskBudget::UNLIMITED,
            RowCount::UNLIMITED,
            WorkUnits::UNLIMITED,
            ElapsedBudget::new(Duration::ZERO),
        ));
        let error = governor
            .check_elapsed_duration(ResourcePhase::Replacement, Duration::from_micros(1))
            .expect_err("nonzero elapsed time must exceed a zero deadline");
        assert_eq!(error.kind(), ResourceKind::Elapsed);
    }

    #[test]
    fn named_governor_publishes_last_operation_telemetry_on_drop() {
        {
            let governor = ResourceGovernor::new_named("query", limits(100));
            governor
                .consume_rows(ResourcePhase::QueryCandidates, RowCount::new(3))
                .expect("row accounting should fit");
            governor
                .consume_work(ResourcePhase::QueryExpand, WorkUnits::new(7))
                .expect("work accounting should fit");
            let _workspace = governor
                .reserve_memory(ResourcePhase::QueryPaths, ByteCount::from_bytes(40))
                .expect("workspace should fit");
        }

        let snapshot = last_operation_snapshot().expect("named operation should be recorded");
        assert_eq!(snapshot.operation, "query");
        assert_eq!(snapshot.memory_budget_bytes, 100);
        assert_eq!(snapshot.memory_peak_bytes, 40);
        assert_eq!(snapshot.memory_peak_phase.as_deref(), Some("query.paths"));
        assert_eq!(snapshot.rows, 3);
        assert_eq!(snapshot.work_units, 7);
    }

    #[test]
    fn row_and_work_breakers_stop_before_crossing_the_limit() {
        let governor = ResourceGovernor::new(ResourceLimits::bounded(
            MemoryBudget::new(ByteCount::from_bytes(1)),
            DiskBudget::UNLIMITED,
            RowCount::new(2),
            WorkUnits::new(3),
            ElapsedBudget::new(Duration::MAX),
        ));
        governor
            .consume_rows(ResourcePhase::SyncIngest, RowCount::new(2))
            .expect("row limit should be inclusive");
        let rows = governor
            .consume_rows(ResourcePhase::SyncIngest, RowCount::new(1))
            .expect_err("row limit should reject the next row");
        assert_eq!(rows.kind(), ResourceKind::Rows);
        assert_eq!(governor.rows_used().as_u64(), 2);

        governor
            .consume_work(ResourcePhase::QueryExpand, WorkUnits::new(3))
            .expect("work limit should be inclusive");
        let work = governor
            .consume_work(ResourcePhase::QueryExpand, WorkUnits::new(1))
            .expect_err("work limit should reject the next unit");
        assert_eq!(work.kind(), ResourceKind::Work);
        assert_eq!(governor.work_used().as_u64(), 3);
    }

    #[test]
    fn disk_leases_track_peak_and_release_capacity() {
        let governor = ResourceGovernor::new(ResourceLimits::bounded(
            MemoryBudget::new(ByteCount::from_bytes(1)),
            DiskBudget::new(ByteCount::from_bytes(10)),
            RowCount::UNLIMITED,
            WorkUnits::UNLIMITED,
            ElapsedBudget::new(Duration::MAX),
        ));
        {
            let _disk = governor
                .reserve_disk(ResourcePhase::CompactionMerge, ByteCount::from_bytes(10))
                .expect("disk limit should be inclusive");
            assert_eq!(governor.disk_peak().as_u64(), 10);
            assert!(governor
                .reserve_disk(ResourcePhase::CompactionMerge, ByteCount::from_bytes(1))
                .is_err());
        }
        governor
            .reserve_disk(ResourcePhase::SyncIngest, ByteCount::from_bytes(10))
            .expect("dropped disk lease should release capacity");
    }

    #[test]
    fn disk_lease_growth_and_explicit_release_preserve_accounting() {
        let governor = ResourceGovernor::new(ResourceLimits::bounded(
            MemoryBudget::new(ByteCount::from_bytes(10)),
            DiskBudget::new(ByteCount::from_bytes(20)),
            RowCount::UNLIMITED,
            WorkUnits::UNLIMITED,
            ElapsedBudget::new(Duration::MAX),
        ));
        let mut lease = governor
            .reserve_disk(ResourcePhase::BuildRunWrite, ByteCount::from_bytes(5))
            .unwrap();
        lease
            .try_grow(ResourcePhase::BuildRunMerge, ByteCount::from_bytes(7))
            .unwrap();
        assert_eq!(lease.amount(), ByteCount::from_bytes(12));
        assert_eq!(governor.disk_peak(), ByteCount::from_bytes(12));
        lease.release_all();
        let replacement = governor
            .reserve_disk(ResourcePhase::BuildRunWrite, ByteCount::from_bytes(20))
            .unwrap();
        assert_eq!(replacement.amount(), ByteCount::from_bytes(20));
    }
}
