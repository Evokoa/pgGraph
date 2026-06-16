use super::admin::with_panic_boundary;
use crate::safety::GraphError;

use pgrx::pg_extern;

/// Execute a SQL/PGQ graph pattern query.
///
/// This endpoint is a placeholder. Full public SQL/PGQ execution is 
/// permanently rejected until PostgreSQL graph-pattern hooks are stable
/// in supported PostgreSQL versions.
#[pg_extern(schema = "graph")]
fn pgq(_query: &str) {
    with_panic_boundary("pgq()", || {
        GraphError::UnsupportedOperation {
            operation: "SQL/PGQ".into(),
            reason: "Public execution awaits upstream PostgreSQL graph-pattern hooks stabilization".into(),
        }
        .report();
    })
}
