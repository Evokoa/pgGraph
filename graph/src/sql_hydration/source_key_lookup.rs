//! Operation-local source predicates; graph identity encoding stays unchanged.

use crate::{acl, builder::PrimaryKeySpec, catalog, resource, safety};
use pgrx::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyType {
    Text,
    Int2,
    Int4,
    Int8,
}

impl KeyType {
    fn from_oid(oid: pgrx::pg_sys::Oid) -> Self {
        match oid {
            pgrx::pg_sys::INT2OID => Self::Int2,
            pgrx::pg_sys::INT4OID => Self::Int4,
            pgrx::pg_sys::INT8OID => Self::Int8,
            _ => Self::Text,
        }
    }
}

/// Prepared only for one Rust hydration/recheck operation, never across SQL
/// statements, role changes, or DDL. Both queries still execute as the caller.
pub(crate) struct SourceKeyLookup {
    pub(crate) table_name: String,
    pub(crate) key_expr: String,
    predicate_expr: String,
    key_type: KeyType,
}

impl SourceKeyLookup {
    pub(crate) fn prepare(
        table_oid: u32,
        columns: &PrimaryKeySpec,
        alias: &str,
        governor: &resource::ResourceGovernor,
    ) -> safety::GraphResult<Self> {
        crate::sql_visibility::postgres_error_as_rust_unwind(std::panic::AssertUnwindSafe(|| {
            // Authorization precedes metadata resolution and all key parsing,
            // including requests containing only invalid integer identities.
            acl::check_table_acl(table_oid)?;
            resource::check_postgres_interrupts();
            let column_bytes = columns.columns().iter().try_fold(0usize, |bytes, column| {
                bytes.checked_add(column.len()).ok_or_else(|| {
                    safety::GraphError::Internal("source lookup size overflowed".into())
                })
            })?;
            // Covers quoted expressions, source metadata, table name, and a cache
            // entry. Include per-column String/vector/syntax overhead for wide
            // composite identities, not just identifier payload and escaping.
            let column_overhead = alias
                .len()
                .checked_mul(4)
                .and_then(|bytes| bytes.checked_add(64))
                .and_then(|bytes| bytes.checked_mul(columns.columns().len()));
            let key_bytes = column_bytes
                .checked_mul(12)
                .and_then(|bytes| bytes.checked_add(column_overhead?))
                .ok_or_else(|| {
                    safety::GraphError::Internal("source lookup size overflowed".into())
                })?;
            let workspace = super::reserve_hydration_workspace(governor, 4, key_bytes)?;
            let lock_mode = pgrx::pg_sys::LOCKMODE::try_from(pgrx::pg_sys::AccessShareLock)
                .map_err(|_| safety::GraphError::Internal("invalid source lock mode".into()))?;
            let oid = pgrx::pg_sys::Oid::from_u32(table_oid);
            // SAFETY: On the active backend thread, the checked lock mode pins
            // this source OID before PgRelation::open, satisfying its lock
            // precondition. PgRelation owns only the relcache reference and
            // closes it on drop. PostgreSQL owns the separate transaction lock;
            // no pointer escapes and the existing boundary handles PG errors.
            let relation = unsafe {
                pgrx::pg_sys::LockRelationOid(oid, lock_mode);
                pgrx::PgRelation::open(oid)
            };
            let key_type = if let [column] = columns.columns() {
                // Relcache metadata is current after a lock wait. A catalog
                // SELECT using the older user snapshot could see the type
                // from before a just-committed ALTER COLUMN TYPE instead.
                let descriptor = relation.tuple_desc();
                governor
                    .consume_work(
                        resource::ResourcePhase::QueryHydrate,
                        resource::WorkUnits::new(u64::try_from(descriptor.len()).map_err(
                            |_| {
                                safety::GraphError::Internal(
                                    "source attribute count overflowed".into(),
                                )
                            },
                        )?),
                    )
                    .map_err(safety::resource_limit_error)?;
                let oid = descriptor
                    .iter()
                    .find(|attribute| {
                        !attribute.attisdropped && name_data_to_str(&attribute.attname) == column
                    })
                    .map(|attribute| attribute.atttypid)
                    .ok_or_else(|| {
                        safety::GraphError::Internal(format!(
                            "registered key column {column} is absent from table OID {table_oid}"
                        ))
                    })?;
                KeyType::from_oid(oid)
            } else {
                KeyType::Text
            };
            // A cached regclass rendering can be unqualified. Always qualify
            // the locked relation so policy or type-output functions changing
            // search_path cannot redirect a later source query to a shadow.
            let table_name = format!(
                "{}.{}",
                crate::quote::quote_ident(relation.namespace()),
                crate::quote::quote_ident(relation.name()),
            );
            drop(relation);
            let key_expr = catalog::primary_key_expr(alias, columns);
            let predicate_expr = if key_type == KeyType::Text {
                key_expr.clone()
            } else {
                format!(
                    "{alias}.{}",
                    crate::quote::quote_ident(&columns.columns()[0])
                )
            };
            workspace.retain_until_governor_drop();
            Ok(Self {
                table_name,
                key_expr,
                predicate_expr,
                key_type,
            })
        }))
    }

    pub(crate) fn scalar_predicate(&self) -> String {
        format!("{} = $1", self.predicate_expr)
    }

    pub(crate) fn batch_predicate(&self) -> String {
        format!("{} = ANY($1)", self.predicate_expr)
    }

    pub(crate) fn scalar_arg<'a>(&self, key: &'a str) -> pgrx::datum::DatumWithOid<'a> {
        // Typed NULL retains the ordinary SQL no-match result for invalid or
        // noncanonical input, without invoking PostgreSQL's permissive casts.
        match self.key_type {
            KeyType::Text => key.into(),
            KeyType::Int2 => canonical_integer(key)
                .and_then(|v| i16::try_from(v).ok())
                .into(),
            KeyType::Int4 => canonical_integer(key)
                .and_then(|v| i32::try_from(v).ok())
                .into(),
            KeyType::Int8 => canonical_integer(key).into(),
        }
    }

    pub(crate) fn batch_arg<'a>(
        &self,
        keys: &'a [String],
        governor: &resource::ResourceGovernor,
    ) -> safety::GraphResult<pgrx::datum::DatumWithOid<'a>> {
        // Existing per-row hydration workspace covers these bounded numeric
        // vectors as well as the text-array fallback.
        Ok(match self.key_type {
            KeyType::Text => keys.to_vec().into(),
            KeyType::Int2 => integer_batch::<i16>(keys, governor)?.into(),
            KeyType::Int4 => integer_batch::<i32>(keys, governor)?.into(),
            KeyType::Int8 => integer_batch::<i64>(keys, governor)?.into(),
        })
    }
}

fn integer_batch<T: TryFrom<i64>>(
    keys: &[String],
    governor: &resource::ResourceGovernor,
) -> safety::GraphResult<Vec<T>> {
    let bytes = keys
        .len()
        .checked_mul(std::mem::size_of::<T>())
        .ok_or_else(|| {
            safety::GraphError::Internal("source lookup batch size overflowed".into())
        })?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(keys.len())
        .map_err(|_| super::hydration_allocation_error(governor, bytes))?;
    for (index, key) in keys.iter().enumerate() {
        if index.is_multiple_of(1_024) {
            crate::sql_visibility::postgres_error_as_rust_unwind(std::panic::AssertUnwindSafe(
                resource::check_postgres_interrupts,
            ));
        }
        if let Some(value) = canonical_integer(key).and_then(|value| T::try_from(value).ok()) {
            values.push(value);
        }
    }
    Ok(values)
}

fn canonical_integer(key: &str) -> Option<i64> {
    if key == "0" {
        return Some(0);
    }
    let digits = key.strip_prefix('-').unwrap_or(key).as_bytes();
    if !matches!(digits.first(), Some(b'1'..=b'9')) || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    key.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_identity_parser_preserves_canonical_text_equality() {
        for value in [
            i64::MIN,
            i64::MAX,
            i32::MIN.into(),
            i32::MAX.into(),
            i16::MIN.into(),
            i16::MAX.into(),
            -1,
            0,
            1,
        ] {
            assert_eq!(canonical_integer(&value.to_string()), Some(value));
        }
        for key in [
            "",
            "-",
            "+1",
            "01",
            "-01",
            "-0",
            "00",
            " 1",
            "1 ",
            "1\n",
            "1.0",
            "1e0",
            "１",
            "9223372036854775808",
            "-9223372036854775809",
        ] {
            assert_eq!(canonical_integer(key), None, "{key:?}");
        }
    }

    #[test]
    fn only_exact_builtin_integer_oids_use_typed_lookups() {
        assert_eq!(KeyType::from_oid(pgrx::pg_sys::INT2OID), KeyType::Int2);
        assert_eq!(KeyType::from_oid(pgrx::pg_sys::INT4OID), KeyType::Int4);
        assert_eq!(KeyType::from_oid(pgrx::pg_sys::INT8OID), KeyType::Int8);
        for oid in [
            pgrx::pg_sys::TEXTOID,
            pgrx::pg_sys::OIDOID,
            pgrx::pg_sys::NUMERICOID,
            pgrx::pg_sys::Oid::from_u32(16_384),
        ] {
            assert_eq!(KeyType::from_oid(oid), KeyType::Text);
        }
    }
}
