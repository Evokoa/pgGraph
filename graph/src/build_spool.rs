//! Bounded PostgreSQL temporary spools shared by graph build pipelines.

use pgrx::prelude::*;

use crate::resource::{AdaptiveBatch, BatchAction, ByteCount, ResourcePhase};
use crate::safety::{GraphError, GraphResult};

/// Bounded writer for the build-local node identity lookup.
pub(crate) struct NodeLookupBatch<'a> {
    table_oids: Vec<i64>,
    primary_keys: Vec<String>,
    node_indices: Vec<i64>,
    policy: AdaptiveBatch,
    memory: crate::resource::ResourceLease<'a>,
    disk: crate::resource::DiskLease<'a>,
}

/// Retains the governed disk reservation while the temporary lookup is live.
pub(crate) struct NodeLookupGuard<'a> {
    _disk: crate::resource::DiskLease<'a>,
}

impl<'a> NodeLookupBatch<'a> {
    pub(crate) fn with_limits(
        governor: &'a crate::resource::ResourceGovernor,
        row_limit: i32,
        byte_limit: ByteCount,
    ) -> GraphResult<Self> {
        Ok(Self {
            table_oids: Vec::new(),
            primary_keys: Vec::new(),
            node_indices: Vec::new(),
            policy: AdaptiveBatch::new(
                ResourcePhase::NodeBatch,
                row_limit.max(1) as usize,
                byte_limit,
            ),
            memory: governor
                .reserve_memory(ResourcePhase::NodeBatch, ByteCount::ZERO)
                .map_err(resource_error_to_graph)?,
            disk: governor
                .reserve_disk(ResourcePhase::NodeBatch, ByteCount::ZERO)
                .map_err(resource_error_to_graph)?,
        })
    }

    pub(crate) fn push(
        &mut self,
        table_oid: u32,
        primary_key: &str,
        node_idx: u32,
    ) -> GraphResult<()> {
        let row_bytes = batch_row_bytes::<(i64, i64)>(primary_key)?;
        if self.policy.before_push(row_bytes) == BatchAction::Flush {
            self.flush()?;
        }
        self.memory
            .try_grow_in(ResourcePhase::NodeBatch, row_bytes)
            .map_err(resource_error_to_graph)?;
        // PostgreSQL temporary heap tuples plus both lookup indexes can exceed
        // the raw payload substantially. Reserve a conservative 4x envelope
        // so the logical spill breaker does not undercount the live spool.
        let spool_bytes = row_bytes
            .checked_mul(4)
            .ok_or_else(|| GraphError::Internal("node lookup disk accounting overflowed".into()))?;
        self.disk
            .try_grow(ResourcePhase::NodeBatch, spool_bytes)
            .map_err(resource_error_to_graph)?;
        self.table_oids
            .try_reserve(1)
            .map_err(|error| allocation_error("node lookup table OIDs", error))?;
        self.primary_keys
            .try_reserve(1)
            .map_err(|error| allocation_error("node lookup primary keys", error))?;
        self.node_indices
            .try_reserve(1)
            .map_err(|error| allocation_error("node lookup indices", error))?;
        self.table_oids.push(i64::from(table_oid));
        let mut owned_key = String::new();
        owned_key
            .try_reserve_exact(primary_key.len())
            .map_err(|error| allocation_error("node lookup primary key", error))?;
        owned_key.push_str(primary_key);
        self.primary_keys.push(owned_key);
        self.node_indices.push(i64::from(node_idx));
        self.policy
            .record(row_bytes)
            .map_err(batch_accounting_error)?;
        if self.policy.is_full() {
            self.flush()?;
        }
        Ok(())
    }

    pub(crate) fn flush(&mut self) -> GraphResult<()> {
        if self.table_oids.is_empty() {
            return Ok(());
        }
        let table_oids = std::mem::take(&mut self.table_oids);
        let primary_keys = std::mem::take(&mut self.primary_keys);
        let node_indices = std::mem::take(&mut self.node_indices);
        self.policy.clear();
        let result = Spi::run_with_args(
            "INSERT INTO pg_temp.graph_build_nodes (table_oid, primary_key, node_idx)
             SELECT table_oid, primary_key, node_idx
             FROM unnest($1::int8[], $2::text[], $3::int8[])
               AS node(table_oid, primary_key, node_idx)",
            &[table_oids.into(), primary_keys.into(), node_indices.into()],
        )
        .map_err(|error| GraphError::Internal(format!("node lookup batch insert failed: {error}")));
        self.memory.release_all();
        result
    }

    pub(crate) fn pressure_events(&self) -> u64 {
        self.policy.pressure_events()
    }

    /// Flush and index the lookup while transferring its disk reservation.
    pub(crate) fn finish(mut self) -> GraphResult<NodeLookupGuard<'a>> {
        self.flush()?;
        index_node_lookup_spool()?;
        Ok(NodeLookupGuard { _disk: self.disk })
    }
}

pub(crate) fn create_node_lookup_spool() -> GraphResult<()> {
    Spi::run(
        "DROP TABLE IF EXISTS pg_temp.graph_build_nodes;
         CREATE TEMP TABLE graph_build_nodes (
            table_oid bigint NOT NULL,
            primary_key text NOT NULL,
            node_idx bigint NOT NULL
         ) ON COMMIT DROP",
    )
    .map_err(|error| GraphError::Internal(format!("node lookup spool setup failed: {error}")))
}

pub(crate) fn index_node_lookup_spool() -> GraphResult<()> {
    Spi::run(
        "CREATE INDEX graph_build_nodes_table_pk_idx
           ON pg_temp.graph_build_nodes (table_oid, primary_key);
         CREATE INDEX graph_build_nodes_pk_idx
           ON pg_temp.graph_build_nodes (primary_key, table_oid);
         ANALYZE pg_temp.graph_build_nodes",
    )
    .map_err(|error| GraphError::Internal(format!("node lookup spool index failed: {error}")))
}

/// Resolve endpoint primary-key pairs against the indexed build lookup.
fn batch_row_bytes<T>(text: &str) -> GraphResult<ByteCount> {
    std::mem::size_of::<T>()
        .checked_add(std::mem::size_of::<String>())
        .and_then(|bytes| bytes.checked_add(text.len()))
        .and_then(ByteCount::from_usize)
        .ok_or_else(|| GraphError::Internal("build batch row size overflowed".into()))
}

fn allocation_error(_area: &str, _error: std::collections::TryReserveError) -> GraphError {
    GraphError::Oom {
        used_mb: 0,
        need_mb: 1,
        limit_mb: crate::config::MEMORY_LIMIT_MB.get().max(1) as u64,
    }
}

fn batch_accounting_error(error: crate::resource::ResourceLimitError) -> GraphError {
    crate::safety::resource_limit_error(error)
}

fn resource_error_to_graph(error: crate::resource::ResourceLimitError) -> GraphError {
    if error.kind() == crate::resource::ResourceKind::Memory {
        GraphError::Oom {
            used_mb: ByteCount::from_bytes(error.used()).ceil_mib(),
            need_mb: ByteCount::from_bytes(error.requested()).ceil_mib(),
            limit_mb: ByteCount::from_bytes(error.limit()).as_u64() / 1_048_576,
        }
    } else {
        crate::safety::resource_limit_error(error)
    }
}
