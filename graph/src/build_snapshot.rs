//! Coherent PostgreSQL source boundary for graph construction.
//!
//! A build holds catalog and source relations in a stable lock order, captures
//! the durable sync watermark only after the writer barrier is exclusive, and
//! revalidates catalog, schema, privileges, and watermark before publication.

use std::collections::BTreeSet;

use pgrx::prelude::*;

use crate::builder::{RegisteredEdge, RegisteredFilterColumn, RegisteredTable};
use crate::{acl, catalog, config, safety, sql_sync};

const CATALOG_RELATIONS: [&str; 4] = [
    "graph._graphs",
    "graph._registered_tables",
    "graph._registered_edges",
    "graph._registered_filter_columns",
];

/// Captured identity of the PostgreSQL rows used by one graph build.
pub(crate) struct SourceSnapshotBoundary {
    catalog_fingerprint: u64,
    sync_watermark: i64,
}

impl SourceSnapshotBoundary {
    /// Lock registered sources and establish the exclusive sync-writer fence.
    pub(crate) fn establish(
        tables: &[RegisteredTable],
        edges: &[RegisteredEdge],
        filter_columns: &[RegisteredFilterColumn],
        sync_mode: config::SyncMode,
        operation: &str,
    ) -> safety::GraphResult<Self> {
        lock_registered_sources(tables, edges)?;
        prepare_sync_boundary(sync_mode, operation)?;
        let (catalog_fingerprint, drift_reason) =
            catalog::current_catalog_state_from_rows(tables, edges, filter_columns)?;
        if let Some(reason) = drift_reason {
            return Err(safety::GraphError::Internal(format!(
                "registered source schema changed before graph.{operation}(): {reason}"
            )));
        }
        check_source_acls(tables, edges)?;
        let sync_watermark = sql_sync::max_sync_log_id()?;
        Ok(Self {
            catalog_fingerprint,
            sync_watermark,
        })
    }

    pub(crate) fn catalog_fingerprint(&self) -> u64 {
        self.catalog_fingerprint
    }

    pub(crate) fn sync_watermark(&self) -> i64 {
        self.sync_watermark
    }

    /// Recheck every mutable boundary immediately before persistence/install.
    pub(crate) fn verify(&self, operation: &str) -> safety::GraphResult<()> {
        let (tables, edges, filter_columns) = catalog::read_catalog()?;
        let (fingerprint, drift_reason) =
            catalog::current_catalog_state_from_rows(&tables, &edges, &filter_columns)?;
        if fingerprint != self.catalog_fingerprint {
            return Err(safety::GraphError::BuildLocked);
        }
        if let Some(reason) = drift_reason {
            return Err(safety::GraphError::Internal(format!(
                "registered source schema changed during graph.{operation}(): {reason}"
            )));
        }
        check_source_acls(&tables, &edges)?;
        if sql_sync::max_sync_log_id()? != self.sync_watermark {
            return Err(safety::GraphError::BuildLocked);
        }
        Ok(())
    }
}

/// Lock the mapping catalog before any build reads its rows.
pub(crate) fn lock_catalog() -> safety::GraphResult<()> {
    reject_current_backend_write_locks(&catalog_relation_oids_sql(), "mapping catalog")?;
    for relation in CATALOG_RELATIONS {
        try_lock_relation(&format!("LOCK TABLE {relation} IN SHARE MODE NOWAIT"))?;
    }
    Ok(())
}

fn lock_registered_sources(
    tables: &[RegisteredTable],
    edges: &[RegisteredEdge],
) -> safety::GraphResult<()> {
    let registered = registered_relation_oids(tables, edges);
    if registered.is_empty() {
        return Ok(());
    }
    let roots = partition_roots(&registered)?;
    reject_current_backend_write_locks(&oid_list(&roots), "registered source")?;
    lock_relations(&roots)?;

    // Holding every root in SHARE prevents concurrent partition attachment or
    // detachment while descendants are discovered and locked.
    let descendants = partition_descendants(&roots)?;
    reject_current_backend_write_locks(&oid_list(&descendants), "source partition")?;
    let root_set: BTreeSet<u32> = roots.iter().copied().collect();
    let children = descendants
        .into_iter()
        .filter(|oid| !root_set.contains(oid))
        .collect::<Vec<_>>();
    lock_relations(&children)
}

fn registered_relation_oids(tables: &[RegisteredTable], edges: &[RegisteredEdge]) -> Vec<u32> {
    let mut oids = BTreeSet::new();
    oids.extend(tables.iter().map(|table| table.table_oid));
    for edge in edges {
        oids.insert(edge.from_table_oid);
        oids.insert(edge.to_table_oid);
    }
    oids.into_iter().collect()
}

fn partition_roots(oids: &[u32]) -> safety::GraphResult<Vec<u32>> {
    read_oid_rows(&format!(
        "SELECT DISTINCT COALESCE(
                            pg_catalog.pg_partition_root(source_oid),
                            source_oid::regclass
                        )::oid::integer
           FROM pg_catalog.unnest(ARRAY[{}]::oid[]) AS source(source_oid)
          ORDER BY 1",
        oid_list(oids)
    ))
}

fn partition_descendants(roots: &[u32]) -> safety::GraphResult<Vec<u32>> {
    read_oid_rows(&format!(
        "WITH RECURSIVE descendants(relation_oid) AS (
             SELECT source_oid
               FROM pg_catalog.unnest(ARRAY[{}]::oid[]) AS source(source_oid)
             UNION
             SELECT inheritance.inhrelid
               FROM pg_catalog.pg_inherits AS inheritance
               JOIN descendants AS parent
                 ON parent.relation_oid = inheritance.inhparent
         )
         SELECT relation_oid::integer FROM descendants ORDER BY relation_oid",
        oid_list(roots)
    ))
}

fn read_oid_rows(query: &str) -> safety::GraphResult<Vec<u32>> {
    Spi::connect(|client| {
        let rows = client.select(query, None, &[]).map_err(|error| {
            safety::GraphError::Internal(format!("source relation discovery failed: {error}"))
        })?;
        rows.into_iter()
            .map(|row| {
                row.get::<i32>(1)
                    .map_err(|error| {
                        safety::GraphError::Internal(format!(
                            "source relation OID read failed: {error}"
                        ))
                    })?
                    .and_then(|oid| u32::try_from(oid).ok())
                    .ok_or_else(|| {
                        safety::GraphError::Internal(
                            "source relation discovery returned an invalid OID".to_string(),
                        )
                    })
            })
            .collect()
    })
}

fn lock_relations(oids: &[u32]) -> safety::GraphResult<()> {
    for oid in oids {
        let relation = qualified_relation_name(*oid)?;
        try_lock_relation(&format!("LOCK TABLE ONLY {relation} IN SHARE MODE NOWAIT"))?;
    }
    Ok(())
}

fn try_lock_relation(query: &str) -> safety::GraphResult<()> {
    let acquired = pgrx::pg_sys::PgTryBuilder::new(|| Spi::run(query).map(|_| true))
        .catch_when(
            pgrx::pg_sys::errcodes::PgSqlErrorCode::ERRCODE_LOCK_NOT_AVAILABLE,
            |_| Ok(false),
        )
        .execute()
        .map_err(|error| {
            safety::GraphError::Internal(format!("source relation lock failed: {error}"))
        })?;
    if acquired {
        Ok(())
    } else {
        Err(safety::GraphError::BuildLocked)
    }
}

fn qualified_relation_name(oid: u32) -> safety::GraphResult<String> {
    let query = format!(
        "SELECT pg_catalog.quote_ident(namespace.nspname) || '.' ||
                pg_catalog.quote_ident(relation.relname)
           FROM pg_catalog.pg_class AS relation
           JOIN pg_catalog.pg_namespace AS namespace
             ON namespace.oid = relation.relnamespace
          WHERE relation.oid = {oid}::oid"
    );
    Spi::get_one::<String>(&query)
        .map_err(|error| {
            safety::GraphError::Internal(format!(
                "source relation name lookup failed for OID {oid}: {error}"
            ))
        })?
        .ok_or_else(|| {
            safety::GraphError::Internal(format!(
                "registered source relation OID {oid} no longer exists; re-register it"
            ))
        })
}

fn reject_current_backend_write_locks(relation_oids: &str, _kind: &str) -> safety::GraphResult<()> {
    // pgrx tests execute setup and assertions in one harness transaction. The
    // production binary always performs this check; production-shaped heavy
    // tests cover its fail-closed behavior without the `pg_test` feature.
    if cfg!(feature = "pg_test") {
        return Ok(());
    }
    let query = format!(
        "SELECT EXISTS (
             SELECT 1
               FROM pg_catalog.pg_locks
              WHERE pid = pg_catalog.pg_backend_pid()
                AND locktype = 'relation'
                AND granted
                AND relation = ANY (ARRAY[{relation_oids}]::oid[])
                AND mode = ANY (ARRAY[
                    'RowExclusiveLock',
                    'ShareUpdateExclusiveLock',
                    'ShareRowExclusiveLock',
                    'ExclusiveLock',
                    'AccessExclusiveLock'
                ]::text[])
         )"
    );
    if Spi::get_one::<bool>(&query)
        .map_err(|error| {
            safety::GraphError::Internal(format!("current backend lock inspection failed: {error}"))
        })?
        .unwrap_or(false)
    {
        return Err(safety::GraphError::BuildLocked);
    }
    Ok(())
}

fn catalog_relation_oids_sql() -> String {
    CATALOG_RELATIONS
        .iter()
        .map(|relation| format!("'{relation}'::regclass::oid"))
        .collect::<Vec<_>>()
        .join(",")
}

fn oid_list(oids: &[u32]) -> String {
    oids.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn prepare_sync_boundary(sync_mode: config::SyncMode, operation: &str) -> safety::GraphResult<()> {
    let writer_barrier_held = match sync_mode {
        config::SyncMode::Manual => {
            sql_sync::remove_sync_triggers()?;
            false
        }
        config::SyncMode::Trigger => {
            let current_barrier = sql_sync::sync_writer_barrier_triggers_current()?;
            if current_barrier {
                sql_sync::acquire_sync_writer_barrier()?;
                sql_sync::ensure_no_current_transaction_sync_rows(0)?;
            }
            let installed = sql_sync::install_sync_triggers()?;
            pgrx::warning!(
                "graph.{operation}(): graph.sync_mode = 'trigger' installed graph sync triggers on {} registered table(s); set graph.sync_mode = 'manual' before graph.{operation}() to opt out",
                installed
            );
            current_barrier
        }
        config::SyncMode::Wal => unreachable!("current_sync_mode rejects reserved wal mode"),
    };
    if !writer_barrier_held {
        sql_sync::acquire_sync_writer_barrier()?;
        sql_sync::ensure_no_current_transaction_sync_rows(0)?;
    }
    Ok(())
}

fn check_source_acls(
    tables: &[RegisteredTable],
    edges: &[RegisteredEdge],
) -> safety::GraphResult<()> {
    for oid in registered_relation_oids(tables, edges) {
        acl::check_table_acl(oid)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{catalog_relation_oids_sql, oid_list, registered_relation_oids};
    use crate::builder::{PrimaryKeySpec, PropertyColumns, RegisteredEdge, RegisteredTable};

    #[test]
    fn registered_relation_oids_are_sorted_and_deduplicated() {
        let tables = vec![RegisteredTable {
            table_oid: 12,
            table_name: "public.nodes".to_string(),
            id_columns: PrimaryKeySpec::from("id"),
            columns: PropertyColumns::from(""),
            tenant_column: None,
        }];
        let edges = vec![RegisteredEdge {
            mapping_id: 1,
            from_table_oid: 12,
            from_table: "public.edges".to_string(),
            from_column: "source_id".to_string(),
            source_key_columns: PrimaryKeySpec::from("id"),
            to_table_oid: 9,
            to_table: "public.nodes".to_string(),
            to_column: "id".to_string(),
            label: "link".to_string(),
            bidirectional: false,
            weight_column: None,
            label_column: None,
        }];
        assert_eq!(registered_relation_oids(&tables, &edges), vec![9, 12]);
        assert_eq!(oid_list(&[9, 12]), "9,12");
    }

    #[test]
    fn catalog_lock_identity_is_explicit_and_stable() {
        assert_eq!(
            catalog_relation_oids_sql(),
            "'graph._graphs'::regclass::oid,'graph._registered_tables'::regclass::oid,'graph._registered_edges'::regclass::oid,'graph._registered_filter_columns'::regclass::oid"
        );
    }
}
