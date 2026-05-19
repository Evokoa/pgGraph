//! # Sync — Trigger-based delta sync
//!
//! Provides SQL trigger functions for INSERT/UPDATE/DELETE propagation.
//! Trigger events are recorded in `graph._sync_log`; callers apply them to the
//! backend-local graph with `graph.apply_sync()` or rebuild the base CSR with
//! `graph.maintenance()`.
//!
//! ## Design
//!
//! - INSERTs add or reactivate nodes in NodeStore and ResolutionIndex.
//! - UPDATEs refresh tenant membership, filter values, and edge overlays.
//! - DELETEs tombstone nodes and add edge-delete overlays when row data is
//!   available.
//!
//! The base CSR is immutable. Edge changes live in the backend-local
//! `edge_buffer` overlay until `graph.maintenance()`, `graph.vacuum()`, or
//! `graph.build()` rebuilds the base graph from source tables.
//!
//! See: `docs/contributor_guide/sync-internals.mdx`

use crate::engine::Engine;
use crate::quote::{quote_ident, quote_literal};
use crate::safety::{GraphError, GraphResult};

pub struct QualifiedTable {
    pub oid: u32,
    pub schema: String,
    pub name: String,
}

pub fn get_qualified_table(oid: u32) -> GraphResult<QualifiedTable> {
    pgrx::Spi::connect(|client| {
        let query = format!(
            "SELECT n.nspname::text, c.relname::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.oid = {}",
            oid
        );
        let result = client
            .select(&query, None, &[])
            .map_err(|e| GraphError::Internal(format!("Failed to resolve OID: {}", e)))?;
        let row = result.first();
        let schema = row
            .get::<String>(1)
            .map_err(|e| GraphError::Internal(e.to_string()))?
            .ok_or_else(|| GraphError::Internal("Missing schema".into()))?;
        let name = row
            .get::<String>(2)
            .map_err(|e| GraphError::Internal(e.to_string()))?
            .ok_or_else(|| GraphError::Internal("Missing table name".into()))?;
        Ok(QualifiedTable { oid, schema, name })
    })
}

pub fn qualified_table_sql(t: &QualifiedTable) -> String {
    format!("{}.{}", quote_ident(&t.schema), quote_ident(&t.name))
}

/// Apply a node INSERT to the engine.
///
/// If a node with the same (table_oid, pk) already exists and is active,
/// this performs an upsert (update in place) to avoid creating stale phantom
/// nodes in the NodeStore.
pub fn sync_insert(
    engine: &mut Engine,
    table_oid: u32,
    pk: &str,
    tenant: Option<&str>,
) -> GraphResult<()> {
    if engine.is_read_only {
        return Err(engine.read_only_error());
    }
    engine.materialize_mmap_node_store_for_sync();

    // Upsert: if this (table_oid, pk) already exists, update in place
    // instead of creating a duplicate node slot.
    if engine.resolve(table_oid, pk).is_some() {
        return sync_update(engine, table_oid, pk, tenant);
    }

    // Add to NodeStore
    let node_idx = engine.node_store.add_node(table_oid, pk.to_string());
    if let Some(tenant) = tenant {
        engine.tenanted_table_oids.insert(table_oid);
        engine.insert_tenant_membership(tenant, node_idx);
    }

    // Add to ResolutionIndex
    engine.resolution_insert(table_oid, pk, node_idx);

    Ok(())
}

/// Apply node metadata from an UPDATE to the engine.
///
/// Updates tenant membership.
pub fn sync_update(
    engine: &mut Engine,
    table_oid: u32,
    pk: &str,
    tenant: Option<&str>,
) -> GraphResult<()> {
    if engine.is_read_only {
        return Err(engine.read_only_error());
    }
    engine.materialize_mmap_node_store_for_sync();

    // Resolve node index
    let node_idx = engine
        .resolve(table_oid, pk)
        .ok_or_else(|| GraphError::NodeNotFound {
            table: format!("{}", table_oid),
            pk: pk.to_string(),
        })?;

    if let Some(tenant) = tenant {
        engine.tenanted_table_oids.insert(table_oid);
        for bitmap in engine.tenant_membership.values_mut() {
            bitmap.remove(node_idx);
        }
        engine.insert_tenant_membership(tenant, node_idx);
    }

    Ok(())
}

/// Apply a primary-key-changing UPDATE to the engine.
///
/// The old key is tombstoned and the new key is inserted as a fresh active node.
/// Edge CSR storage is immutable. Callers add delete/insert edge overlays from
/// old and new row images before invoking this helper.
pub fn sync_replace_pk(
    engine: &mut Engine,
    table_oid: u32,
    old_pk: &str,
    new_pk: &str,
    tenant: Option<&str>,
) -> GraphResult<()> {
    if old_pk == new_pk {
        return sync_update(engine, table_oid, new_pk, tenant);
    }

    sync_delete(engine, table_oid, old_pk)?;
    sync_insert(engine, table_oid, new_pk, tenant)
}

/// Apply a node DELETE to the engine.
///
/// Tombstones the node (sets is_active = false).
/// The node slot remains allocated until the next vacuum/rebuild.
pub fn sync_delete(engine: &mut Engine, table_oid: u32, pk: &str) -> GraphResult<()> {
    if engine.is_read_only {
        return Err(engine.read_only_error());
    }
    engine.materialize_mmap_node_store_for_sync();

    // Resolve node index
    let node_idx = engine
        .resolve(table_oid, pk)
        .ok_or_else(|| GraphError::NodeNotFound {
            table: format!("{}", table_oid),
            pk: pk.to_string(),
        })?;

    // Tombstone: mark as inactive
    engine.node_store.deactivate(node_idx);

    for bitmap in engine.tenant_membership.values_mut() {
        bitmap.remove(node_idx);
    }

    Ok(())
}

/// Apply a table-level TRUNCATE event.
pub fn sync_truncate(engine: &mut Engine, table_oid: u32) -> GraphResult<u64> {
    if engine.is_read_only {
        return Err(engine.read_only_error());
    }
    engine.materialize_mmap_node_store_for_sync();
    let mut tombstoned = 0;
    let mut truncated_nodes = Vec::new();
    for node_idx in 0..engine.node_store.node_count() {
        if engine.node_store.table_oid(node_idx) == table_oid
            && engine.node_store.is_active(node_idx)
        {
            engine.node_store.deactivate(node_idx);
            truncated_nodes.push(node_idx);
            tombstoned += 1;
        }
    }
    for bitmap in engine.tenant_membership.values_mut() {
        for &node_idx in &truncated_nodes {
            bitmap.remove(node_idx);
        }
    }
    engine.needs_vacuum = true;
    Ok(tombstoned)
}

/// Generate the SQL for creating trigger functions on a registered table.
///
/// For composite PKs (id_column contains ','), the PK expression uses
/// `jsonb_build_array(NEW."col1"::text, NEW."col2"::text)::text` to produce
/// a JSON array string matching the builder's format.
pub fn generate_trigger_sql(qt: &QualifiedTable, id_column: &str, columns: &[String]) -> String {
    let table_sql = qualified_table_sql(qt);
    let trigger_fn_name = format!("_sync_{}", qt.oid);

    let key_val_pairs_new = columns
        .iter()
        .map(|c| format!("{}, NEW.{}::text", quote_literal(c), quote_ident(c)))
        .collect::<Vec<_>>()
        .join(", ");

    // Build PK expressions for NEW and OLD references
    let (new_pk_expr, old_pk_expr) = if id_column.contains(',') {
        // Composite PK: jsonb_build_array(NEW."col1"::text, NEW."col2"::text)::text
        let pk_cols: Vec<&str> = id_column.split(',').map(|s| s.trim()).collect();
        let new_parts: Vec<String> = pk_cols
            .iter()
            .map(|c| format!("NEW.{}::text", quote_ident(c)))
            .collect();
        let old_parts: Vec<String> = pk_cols
            .iter()
            .map(|c| format!("OLD.{}::text", quote_ident(c)))
            .collect();
        (
            format!("jsonb_build_array({})::text", new_parts.join(", ")),
            format!("jsonb_build_array({})::text", old_parts.join(", ")),
        )
    } else {
        // Single PK: NEW."id"::text
        (
            format!("NEW.{}::text", quote_ident(id_column)),
            format!("OLD.{}::text", quote_ident(id_column)),
        )
    };

    format!(
        r#"
-- Trigger function for {table_sql}
CREATE OR REPLACE FUNCTION graph.{trigger_fn_name}()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        INSERT INTO graph._sync_log
            (op, table_oid, table_name, pk, old_pk, new_pk, properties, old_row, new_row, xid)
        VALUES
            ('I', {table_oid}, {table_name_lit}, {new_pk_expr}, NULL, {new_pk_expr},
             jsonb_build_object({key_val_pairs_new}), NULL, to_jsonb(NEW), txid_current());
        RETURN NEW;
    ELSIF TG_OP = 'UPDATE' THEN
        INSERT INTO graph._sync_log
            (op, table_oid, table_name, pk, old_pk, new_pk, properties, old_row, new_row, xid)
        VALUES
            ('U', {table_oid}, {table_name_lit}, {new_pk_expr}, {old_pk_expr}, {new_pk_expr},
             jsonb_build_object({key_val_pairs_new}), to_jsonb(OLD), to_jsonb(NEW), txid_current());
        RETURN NEW;
    ELSIF TG_OP = 'DELETE' THEN
        INSERT INTO graph._sync_log
            (op, table_oid, table_name, pk, old_pk, new_pk, properties, old_row, new_row, xid)
        VALUES
            ('D', {table_oid}, {table_name_lit}, {old_pk_expr}, {old_pk_expr}, NULL,
             NULL, to_jsonb(OLD), NULL, txid_current());
        RETURN OLD;
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION graph.{trigger_fn_name}_truncate()
RETURNS TRIGGER AS $$
BEGIN
    INSERT INTO graph._sync_log
        (op, table_oid, table_name, xid, needs_vacuum)
    VALUES
        ('T', {table_oid}, {table_name_lit}, txid_current(), true);
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

-- Attach triggers
DROP TRIGGER IF EXISTS graph_sync_insert ON {table_sql};
CREATE TRIGGER graph_sync_insert
    AFTER INSERT ON {table_sql}
    FOR EACH ROW EXECUTE FUNCTION graph.{trigger_fn_name}();

DROP TRIGGER IF EXISTS graph_sync_update ON {table_sql};
CREATE TRIGGER graph_sync_update
    AFTER UPDATE ON {table_sql}
    FOR EACH ROW EXECUTE FUNCTION graph.{trigger_fn_name}();

DROP TRIGGER IF EXISTS graph_sync_delete ON {table_sql};
CREATE TRIGGER graph_sync_delete
    AFTER DELETE ON {table_sql}
    FOR EACH ROW EXECUTE FUNCTION graph.{trigger_fn_name}();

DROP TRIGGER IF EXISTS graph_sync_truncate ON {table_sql};
CREATE TRIGGER graph_sync_truncate
    AFTER TRUNCATE ON {table_sql}
    FOR EACH STATEMENT EXECUTE FUNCTION graph.{trigger_fn_name}_truncate();
"#,
        table_sql = table_sql,
        trigger_fn_name = trigger_fn_name,
        table_oid = qt.oid,
        table_name_lit = quote_literal(&table_sql),
        new_pk_expr = new_pk_expr,
        old_pk_expr = old_pk_expr,
        key_val_pairs_new = key_val_pairs_new,
    )
}

#[cfg(test)]
mod tests {
    //! Covers trigger SQL generation and sync operation replay so buffered
    //! changes remain ordered, idempotent, and lossless on failure.

    use super::*;
    use crate::engine::Engine;
    use proptest::prelude::*;
    use std::collections::BTreeSet;

    fn test_engine() -> Engine {
        Engine::new()
    }

    fn test_region_for_slices(slices: &[(*const u8, usize)]) -> (*const u8, usize) {
        let start = slices
            .iter()
            .map(|(ptr, _)| *ptr as usize)
            .min()
            .expect("test region requires at least one slice");
        let end = slices
            .iter()
            .map(|(ptr, len)| (*ptr as usize) + len)
            .max()
            .expect("test region requires at least one slice");
        (start as *const u8, end - start)
    }

    #[test]
    fn generate_trigger_sql_quotes_mixed_case_and_reserved_identifiers() {
        let qt = QualifiedTable {
            oid: 42,
            schema: "Weird Schema".to_string(),
            name: "select".to_string(),
        };
        let sql = generate_trigger_sql(
            &qt,
            "Tenant ID, User ID",
            &["Display Name".to_string(), "order".to_string()],
        );

        assert!(sql.contains(r#""Weird Schema"."select""#));
        assert!(sql.contains(r#"NEW."Tenant ID"::text"#));
        assert!(sql.contains(r#"OLD."User ID"::text"#));
        assert!(sql.contains(r#"'Display Name', NEW."Display Name"::text"#));
        assert!(sql.contains(r#"'order', NEW."order"::text"#));
        assert!(sql.contains(r##"'"Weird Schema"."select"'"##));
    }

    // ─── INSERT ───

    #[test]
    fn insert_adds_node_to_all_stores() {
        let mut eng = test_engine();
        sync_insert(&mut eng, 42, "U-1", None).unwrap();

        // Node exists in NodeStore
        assert_eq!(eng.node_store.node_count(), 1);
        assert!(eng.node_store.is_active(0));
        assert_eq!(eng.node_store.primary_key(0), "U-1");
        assert_eq!(eng.node_store.table_oid(0), 42);

        // Node is resolvable
        assert_eq!(eng.resolve(42, "U-1"), Some(0));

        // Source-table SQL owns property search; sync only maintains graph stores.
    }

    #[test]
    fn insert_multiple_nodes_assigns_sequential_indices() {
        let mut eng = test_engine();
        sync_insert(&mut eng, 42, "A", None).unwrap();
        sync_insert(&mut eng, 42, "B", None).unwrap();
        sync_insert(&mut eng, 43, "C", None).unwrap();

        assert_eq!(eng.node_store.node_count(), 3);
        assert_eq!(eng.resolve(42, "A"), Some(0));
        assert_eq!(eng.resolve(42, "B"), Some(1));
        assert_eq!(eng.resolve(43, "C"), Some(2));
    }

    // ─── UPDATE ───

    #[test]
    fn update_existing_node_succeeds() {
        let mut eng = test_engine();
        sync_insert(&mut eng, 42, "U-1", None).unwrap();

        sync_update(&mut eng, 42, "U-1", None).unwrap();

        assert_eq!(eng.resolve(42, "U-1"), Some(0));
    }

    #[test]
    fn update_nonexistent_node_returns_error() {
        let mut eng = test_engine();

        let result = sync_update(&mut eng, 42, "GHOST", None);
        assert!(result.is_err());

        match result.unwrap_err() {
            GraphError::NodeNotFound { table, pk } => {
                assert_eq!(table, "42");
                assert_eq!(pk, "GHOST");
            }
            other => panic!("expected NodeNotFound, got {:?}", other),
        }
    }

    #[test]
    fn replace_pk_tombstones_old_node_and_inserts_new_node() {
        let mut eng = test_engine();
        sync_insert(&mut eng, 42, "old", None).unwrap();

        sync_replace_pk(&mut eng, 42, "old", "new", None).unwrap();

        assert_eq!(eng.node_store.node_count(), 2);
        assert!(!eng.node_store.is_active(0));
        assert!(eng.node_store.is_active(1));
        assert_eq!(eng.resolve(42, "old"), None);
        assert_eq!(eng.resolve(42, "new"), Some(1));
    }

    // ─── DELETE ───

    #[test]
    fn delete_tombstones_node() {
        let mut eng = test_engine();
        sync_insert(&mut eng, 42, "U-1", None).unwrap();
        assert!(eng.node_store.is_active(0));

        sync_delete(&mut eng, 42, "U-1").unwrap();

        // Node is tombstoned but still exists
        assert_eq!(eng.node_store.node_count(), 1);
        assert!(!eng.node_store.is_active(0));

        // Source-table SQL owns property search; delete tombstones the graph slot.
    }

    #[test]
    fn delete_nonexistent_node_returns_error() {
        let mut eng = test_engine();

        let result = sync_delete(&mut eng, 42, "GHOST");
        assert!(result.is_err());

        match result.unwrap_err() {
            GraphError::NodeNotFound { pk, .. } => {
                assert_eq!(pk, "GHOST");
            }
            other => panic!("expected NodeNotFound, got {:?}", other),
        }
    }

    // ─── Lifecycle ───

    #[test]
    fn insert_update_delete_full_lifecycle() {
        let mut eng = test_engine();

        // Insert
        sync_insert(&mut eng, 10, "item-1", None).unwrap();
        assert_eq!(eng.node_store.active_count(), 1);

        // Update
        sync_update(&mut eng, 10, "item-1", None).unwrap();
        assert_eq!(eng.node_store.active_count(), 1);

        // Delete
        sync_delete(&mut eng, 10, "item-1").unwrap();
        assert_eq!(eng.node_store.active_count(), 0);
        assert_eq!(eng.node_store.node_count(), 1); // Slot still allocated

        // Deleted node's graph slot is inactive.
        assert!(!eng.node_store.is_active(0));
    }

    #[test]
    fn delete_does_not_affect_other_nodes() {
        let mut eng = test_engine();
        sync_insert(&mut eng, 42, "keep", None).unwrap();
        sync_insert(&mut eng, 42, "drop", None).unwrap();

        sync_delete(&mut eng, 42, "drop").unwrap();

        // "keep" is still active
        assert!(eng.node_store.is_active(0));
        assert!(!eng.node_store.is_active(1));

        // "keep" remains active and resolvable.
        assert_eq!(eng.resolve(42, "keep"), Some(0));
    }

    #[test]
    fn insert_empty_pk_is_valid() {
        let mut eng = test_engine();
        sync_insert(&mut eng, 42, "", None).unwrap();
        assert_eq!(eng.node_store.primary_key(0), "");
        assert_eq!(eng.resolve(42, ""), Some(0));
    }

    #[test]
    fn duplicate_insert_upserts_existing_node() {
        let mut eng = test_engine();
        sync_insert(&mut eng, 42, "DUP", None).unwrap();
        sync_insert(&mut eng, 42, "DUP", None).unwrap();
        // Upsert: same node slot, properties updated in place
        assert_eq!(eng.node_store.node_count(), 1);
        // Resolution still points to the same node
        assert_eq!(eng.resolve(42, "DUP"), Some(0));
    }

    proptest! {
        #[test]
        fn random_sync_sequences_match_active_model(ops in proptest::collection::vec((0u8..4, 0u8..8, 0u8..8), 1..200)) {
            let mut eng = test_engine();
            let mut active = BTreeSet::new();

            for (kind, from, to) in ops {
                let from_pk = format!("id-{from}");
                let to_pk = format!("id-{to}");
                match kind {
                    0 if !active.contains(&from_pk) => {
                        sync_insert(&mut eng, 42, &from_pk, None).unwrap();
                        active.insert(from_pk);
                    }
                    1 if active.contains(&from_pk) => {
                        sync_update(&mut eng, 42, &from_pk, None).unwrap();
                    }
                    2 if active.contains(&from_pk) => {
                        sync_delete(&mut eng, 42, &from_pk).unwrap();
                        active.remove(&from_pk);
                    }
                    3 if active.contains(&from_pk) && !active.contains(&to_pk) => {
                        sync_replace_pk(&mut eng, 42, &from_pk, &to_pk, None).unwrap();
                        active.remove(&from_pk);
                        active.insert(to_pk);
                    }
                    _ => {}
                }
            }

            prop_assert_eq!(eng.node_store.active_count() as usize, active.len());
            for id in 0..8 {
                let pk = format!("id-{id}");
                prop_assert_eq!(eng.resolve(42, &pk).is_some(), active.contains(&pk));
            }
        }
    }

    #[test]
    fn sync_insert_on_mmap_store_materializes_owned_overlay() {
        let mut eng = test_engine();
        let active = [0u8];
        let oids = [0u32];
        let pk_offsets = [0u64];
        let pk_bytes = [0u8];
        let (region_ptr, region_len) = test_region_for_slices(&[
            (active.as_ptr(), 0),
            (oids.as_ptr().cast::<u8>(), 0),
            (
                pk_offsets.as_ptr().cast::<u8>(),
                pk_offsets.len() * std::mem::size_of::<u64>(),
            ),
            (pk_bytes.as_ptr(), 0),
        ]);
        // SAFETY: Pointers reference local arrays that outlive this test store.
        let arrays = unsafe {
            crate::node_store::MmapNodeArrays::new(crate::node_store::MmapNodeArrayParts {
                region_ptr,
                region_len,
                active_ptr: active.as_ptr(),
                oid_ptr: oids.as_ptr(),
                pk_offsets_ptr: pk_offsets.as_ptr(),
                pk_bytes_ptr: pk_bytes.as_ptr(),
                node_count: 0,
                active_byte_count: 0,
                pk_bytes_len: 0,
            })
            .expect("valid mmap node metadata")
        };
        // SAFETY: The validated metadata above outlives this test store.
        eng.node_store = unsafe { crate::node_store::NodeStore::from_mmap(arrays) };

        let result = sync_insert(&mut eng, 42, "U-1", None);
        assert!(result.is_ok());
        assert!(!eng.node_store.is_mmap_backed());
        assert!(eng.resolve(42, "U-1").is_some());
    }

    #[test]
    fn sync_insert_on_read_only_engine_reports_read_only_reason() {
        let mut eng = test_engine();
        eng.mark_read_only(crate::engine::ReadOnlyReason::MemoryLimit);

        let result = sync_insert(&mut eng, 42, "U-1", None);

        assert!(matches!(
            result,
            Err(GraphError::ReadOnly { reason }) if reason == "memory_limit"
        ));
    }

    #[test]
    fn sync_truncate_on_mmap_store_materializes_owned_overlay() {
        let mut eng = test_engine();
        let active = [0u8];
        let oids = [0u32];
        let pk_offsets = [0u64];
        let pk_bytes = [0u8];
        let (region_ptr, region_len) = test_region_for_slices(&[
            (active.as_ptr(), 0),
            (oids.as_ptr().cast::<u8>(), 0),
            (
                pk_offsets.as_ptr().cast::<u8>(),
                pk_offsets.len() * std::mem::size_of::<u64>(),
            ),
            (pk_bytes.as_ptr(), 0),
        ]);
        // SAFETY: Pointers reference local arrays that outlive this test store.
        let arrays = unsafe {
            crate::node_store::MmapNodeArrays::new(crate::node_store::MmapNodeArrayParts {
                region_ptr,
                region_len,
                active_ptr: active.as_ptr(),
                oid_ptr: oids.as_ptr(),
                pk_offsets_ptr: pk_offsets.as_ptr(),
                pk_bytes_ptr: pk_bytes.as_ptr(),
                node_count: 0,
                active_byte_count: 0,
                pk_bytes_len: 0,
            })
            .expect("valid mmap node metadata")
        };
        // SAFETY: The validated metadata above outlives this test store.
        eng.node_store = unsafe { crate::node_store::NodeStore::from_mmap(arrays) };

        let result = sync_truncate(&mut eng, 42);
        assert_eq!(result.unwrap(), 0);
        assert!(!eng.node_store.is_mmap_backed());
    }

    #[test]
    fn generate_trigger_sql_contains_table_references() {
        let qt = QualifiedTable {
            oid: 12345,
            schema: "public".into(),
            name: "users".into(),
        };
        let sql = generate_trigger_sql(&qt, "id", &["name".to_string(), "email".to_string()]);
        assert!(
            sql.contains("\"public\".\"users\""),
            "should reference table name"
        );
        assert!(sql.contains("NEW.\"id\""), "should reference ID column");
        assert!(sql.contains("'name'"), "should reference columns");
        assert!(sql.contains("'email'"), "should reference columns");
        assert!(sql.contains("old_pk"), "should capture old primary key");
        assert!(sql.contains("new_pk"), "should capture new primary key");
        assert!(
            sql.contains("OLD.\"id\""),
            "should reference OLD primary key for updates/deletes"
        );
        assert!(sql.contains("AFTER INSERT"), "should create insert trigger");
        assert!(sql.contains("AFTER UPDATE"), "should create update trigger");
        assert!(sql.contains("AFTER DELETE"), "should create delete trigger");
    }
}
