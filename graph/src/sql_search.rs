//! SQL-layer search validation and source-row verification helpers.

use crate::api_types::SearchOutputRow;
use crate::catalog::{
    primary_key_expr, read_catalog, relation_name, sql_table_name_from_oid, validate_column_exists,
};
use crate::quote::quote_ident;
use crate::{acl, safety, types};
use pgrx::prelude::*;
use std::borrow::Cow;

struct SourceSearchStatement {
    table_oid: u32,
    table_name: String,
    display_table_name: String,
    query: String,
    size_query: String,
    params: Vec<String>,
}

enum PreparedSearchExpected<'a> {
    Text(Cow<'a, str>),
    Tokens(Vec<String>),
}

struct SearchValueMatcher<'a> {
    mode: types::SearchMode,
    case_sensitive: bool,
    expected: PreparedSearchExpected<'a>,
}

impl<'a> SearchValueMatcher<'a> {
    fn new(expected: &'a str, mode: types::SearchMode, case_sensitive: bool) -> Self {
        let comparable_expected = if case_sensitive {
            Cow::Borrowed(expected)
        } else {
            Cow::Owned(expected.to_lowercase())
        };
        let expected = match mode {
            types::SearchMode::Token => PreparedSearchExpected::Tokens(
                comparable_expected
                    .split(|ch: char| !ch.is_alphanumeric())
                    .filter(|token| !token.is_empty())
                    .map(str::to_owned)
                    .collect(),
            ),
            _ => PreparedSearchExpected::Text(comparable_expected),
        };

        Self {
            mode,
            case_sensitive,
            expected,
        }
    }

    fn matches(&self, actual: &str) -> bool {
        let actual = if self.case_sensitive {
            Cow::Borrowed(actual)
        } else {
            Cow::Owned(actual.to_lowercase())
        };

        match (&self.expected, self.mode) {
            (PreparedSearchExpected::Text(expected), types::SearchMode::Contains) => {
                actual.contains(expected.as_ref())
            }
            (PreparedSearchExpected::Text(expected), types::SearchMode::Exact) => {
                actual == expected.as_ref()
            }
            (PreparedSearchExpected::Text(expected), types::SearchMode::Prefix) => {
                actual.starts_with(expected.as_ref())
            }
            (PreparedSearchExpected::Tokens(expected_tokens), types::SearchMode::Token) => {
                expected_tokens.iter().all(|expected| {
                    actual
                        .split(|ch: char| !ch.is_alphanumeric())
                        .filter(|token| !token.is_empty())
                        .any(|token| token == expected)
                })
            }
            _ => false,
        }
    }
}

pub(crate) fn validate_search_request(
    property_key: &str,
    table_filter: Option<u32>,
    _tenant: Option<&str>,
) -> safety::GraphResult<()> {
    let (tables, _edges, _filter_columns) = read_catalog()?;
    let mut saw_registered_property = false;
    for table in &tables {
        if !table.columns.iter().any(|column| column == property_key) {
            continue;
        }
        let oid = table.table_oid;
        if table_filter.is_none_or(|filter| filter == oid) {
            saw_registered_property = true;
            acl::check_table_acl(oid)?;
            validate_column_exists(oid, property_key)?;
        }
    }

    if let Some(filter) = table_filter {
        acl::check_table_acl(filter)?;
        if !saw_registered_property {
            validate_column_exists(filter, property_key)?;
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn source_table_search_statements(
    property_key: &str,
    property_value: &str,
    table_filter: Option<u32>,
    mode: types::SearchMode,
    case_sensitive: bool,
    tenant: Option<&str>,
    hydrate: bool,
    candidate_limit: Option<usize>,
    mut workspace: Option<&mut crate::resource::ResourceLease<'_>>,
) -> safety::GraphResult<Vec<SourceSearchStatement>> {
    let (tables, _edges, _filter_columns) = read_catalog()?;
    let mut statements = Vec::new();

    for table in tables {
        if !table.columns.iter().any(|column| column == property_key) {
            continue;
        }
        let table_oid = table.table_oid;
        if table_filter.is_some_and(|filter| filter != table_oid) {
            continue;
        }

        if let Some(workspace) = workspace.as_deref_mut() {
            let statement_bytes = search_statement_workspace_bound(
                property_key,
                property_value,
                tenant,
                mode,
                table.id_columns.columns(),
                table.tenant_column.as_deref(),
            )?;
            workspace
                .try_grow(statement_bytes)
                .map_err(crate::safety::resource_limit_error)?;
        }

        let table_name = sql_table_name_from_oid(table_oid)?;
        acl::check_table_acl(table_oid)?;
        validate_column_exists(table_oid, property_key)?;

        let pk_expr = primary_key_expr("src", &table.id_columns);
        let value_expr = format!("src.{}::text", quote_ident(property_key));
        let node_select = if hydrate {
            "to_jsonb(src.*)"
        } else {
            "NULL::jsonb"
        };
        let node_size_select = if hydrate {
            "pg_catalog.pg_column_size(pg_catalog.to_jsonb(src.*))::bigint AS graph_json_binary_bytes,
             pg_catalog.octet_length(pg_catalog.to_jsonb(src.*)::text)::bigint AS graph_json_text_bytes"
        } else {
            "0::bigint AS graph_json_binary_bytes, 0::bigint AS graph_json_text_bytes"
        };
        let mut params = Vec::new();
        let (search_predicate, mut search_params) =
            search_sql_predicate(&value_expr, property_value, mode, case_sensitive, 1);
        params.append(&mut search_params);
        let mut predicates = vec![
            format!("src.{} IS NOT NULL", quote_ident(property_key)),
            search_predicate,
        ];
        if let (Some(tenant), Some(tenant_column)) = (tenant, table.tenant_column.as_deref()) {
            validate_column_exists(table_oid, tenant_column)?;
            predicates.push(format!(
                "src.{}::text = ${}",
                quote_ident(tenant_column),
                params.len() + 1
            ));
            params.push(tenant.to_string());
        }

        let limit_clause = candidate_limit
            .map(|limit| format!("\n             LIMIT {}", limit))
            .unwrap_or_default();
        let query = format!(
            "SELECT {} AS graph_node_id, {} AS graph_value, {} AS graph_node
             FROM {} src
             WHERE {}
             ORDER BY graph_node_id{}",
            pk_expr,
            value_expr,
            node_select,
            table_name.as_sql(),
            predicates.join(" AND "),
            limit_clause
        );
        let size_query = format!(
            "SELECT pg_catalog.count(*)::bigint,
                    COALESCE(pg_catalog.sum(pg_catalog.octet_length(graph_node_id)), 0)::bigint,
                    COALESCE(pg_catalog.sum(pg_catalog.octet_length(graph_value)), 0)::bigint,
                    COALESCE(pg_catalog.max(pg_catalog.octet_length(graph_value)), 0)::bigint,
                    COALESCE(pg_catalog.sum(graph_json_binary_bytes), 0)::bigint,
                    COALESCE(pg_catalog.sum(graph_json_text_bytes), 0)::bigint
               FROM (
                 SELECT {pk_expr} AS graph_node_id,
                        {value_expr} AS graph_value,
                        {node_size_select}
                   FROM {table_name} src
                  WHERE {predicates}
                  ORDER BY graph_node_id{limit_clause}
               ) search_candidates",
            table_name = table_name.as_sql(),
            predicates = predicates.join(" AND "),
        );

        statements.try_reserve(1).map_err(|error| {
            safety::GraphError::Internal(format!(
                "search statement vector allocation failed: {error}"
            ))
        })?;
        statements.push(SourceSearchStatement {
            table_oid,
            table_name: table_name.as_sql().to_string(),
            display_table_name: relation_name(table_oid).unwrap_or_else(|_| table_oid.to_string()),
            query,
            size_query,
            params,
        });
    }

    Ok(statements)
}

fn search_statement_workspace_bound(
    property_key: &str,
    property_value: &str,
    tenant: Option<&str>,
    mode: types::SearchMode,
    id_columns: &[String],
    tenant_column: Option<&str>,
) -> safety::GraphResult<crate::resource::ByteCount> {
    let token_count = if mode == types::SearchMode::Token {
        property_value
            .split(|ch: char| !ch.is_alphanumeric())
            .filter(|token| !token.is_empty())
            .count()
            .max(1)
    } else {
        1
    };
    let quoted_key_bound = property_key
        .len()
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(32));
    let repeated_predicate_bytes = quoted_key_bound
        .and_then(|bytes| bytes.checked_add(128))
        .and_then(|bytes| bytes.checked_mul(token_count))
        .and_then(|bytes| bytes.checked_mul(8));
    let id_bytes = id_columns.iter().try_fold(0usize, |bytes, column| {
        bytes.checked_add(column.len().saturating_mul(4))
    });
    let bytes = property_value
        .len()
        .checked_mul(32)
        .and_then(|bytes| bytes.checked_add(property_key.len().saturating_mul(16)))
        .and_then(|bytes| bytes.checked_add(tenant.map_or(0, str::len).saturating_mul(16)))
        .and_then(|bytes| bytes.checked_add(tenant_column.map_or(0, str::len).saturating_mul(8)))
        .and_then(|bytes| bytes.checked_add(id_bytes?))
        .and_then(|bytes| bytes.checked_add(repeated_predicate_bytes?))
        .and_then(|bytes| bytes.checked_add(64 * 1_024))
        .and_then(crate::resource::ByteCount::from_usize)
        .ok_or_else(|| {
            safety::GraphError::Internal("search statement workspace overflowed".to_string())
        })?;
    Ok(bytes)
}

#[cfg(feature = "pg_test")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn source_table_search_sql_and_params_for_test(
    property_key: &str,
    property_value: &str,
    table_filter: Option<u32>,
    mode: types::SearchMode,
    case_sensitive: bool,
    tenant: Option<&str>,
    hydrate: bool,
) -> safety::GraphResult<Vec<(String, Vec<String>)>> {
    Ok(source_table_search_statements(
        property_key,
        property_value,
        table_filter,
        mode,
        case_sensitive,
        tenant,
        hydrate,
        None,
        None,
    )?
    .into_iter()
    .map(|statement| (statement.query, statement.params))
    .collect())
}

#[cfg(feature = "pg_test")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn source_table_search_sql_for_test(
    property_key: &str,
    property_value: &str,
    table_filter: Option<u32>,
    mode: types::SearchMode,
    case_sensitive: bool,
    tenant: Option<&str>,
    hydrate: bool,
) -> safety::GraphResult<Vec<String>> {
    Ok(source_table_search_statements(
        property_key,
        property_value,
        table_filter,
        mode,
        case_sensitive,
        tenant,
        hydrate,
        None,
        None,
    )?
    .into_iter()
    .map(|statement| statement.query)
    .collect())
}

#[allow(clippy::too_many_arguments)]
#[allow(dead_code, reason = "compatibility entry point")]
pub(crate) fn source_table_search_rows(
    property_key: &str,
    property_value: &str,
    table_filter: Option<u32>,
    mode: types::SearchMode,
    case_sensitive: bool,
    tenant: Option<&str>,
    hydrate: bool,
    offset: usize,
    limit: usize,
) -> safety::GraphResult<Vec<SearchOutputRow>> {
    let governor = crate::ENGINE.with(|engine| engine.borrow().query_resource_governor())?;
    source_table_search_rows_governed(
        property_key,
        property_value,
        table_filter,
        mode,
        case_sensitive,
        tenant,
        hydrate,
        offset,
        limit,
        &governor,
    )
}

/// Searches source tables within a caller-owned operation budget.
#[allow(clippy::too_many_arguments)]
pub(crate) fn source_table_search_rows_governed(
    property_key: &str,
    property_value: &str,
    table_filter: Option<u32>,
    mode: types::SearchMode,
    case_sensitive: bool,
    tenant: Option<&str>,
    hydrate: bool,
    offset: usize,
    limit: usize,
    governor: &crate::resource::ResourceGovernor,
) -> safety::GraphResult<Vec<SearchOutputRow>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let candidate_limit =
        offset
            .checked_add(limit)
            .ok_or_else(|| safety::GraphError::InvalidFilter {
                reason: "row_offset + max_rows overflows".to_string(),
            })?;
    let mut workspace = governor
        .reserve_memory(
            crate::resource::ResourcePhase::QueryCandidates,
            crate::resource::ByteCount::ZERO,
        )
        .map_err(crate::safety::resource_limit_error)?;
    let matcher_bytes = search_matcher_workspace_bound(property_value, mode, case_sensitive)?;
    workspace
        .try_grow(matcher_bytes)
        .map_err(crate::safety::resource_limit_error)?;
    let statements = source_table_search_statements(
        property_key,
        property_value,
        table_filter,
        mode,
        case_sensitive,
        tenant,
        hydrate,
        Some(candidate_limit),
        Some(&mut workspace),
    )?;
    let mut rows = Vec::new();
    let matcher = SearchValueMatcher::new(property_value, mode, case_sensitive);
    let match_type = mode.as_match_type().to_string();

    for statement in statements {
        governor
            .check_elapsed(crate::resource::ResourcePhase::QueryCandidates)
            .map_err(crate::safety::resource_limit_error)?;
        crate::resource::check_postgres_interrupts();
        governor
            .consume_work(
                crate::resource::ResourcePhase::QueryCandidates,
                crate::resource::WorkUnits::new(1),
            )
            .map_err(crate::safety::resource_limit_error)?;
        let sizes = search_statement_sizes(&statement)?;
        reserve_search_materialization(
            &mut workspace,
            sizes,
            statement.display_table_name.len(),
            match_type.len(),
        )?;
        let statement_rows = usize::try_from(sizes.rows).map_err(|_| {
            safety::GraphError::Internal(
                "search row count does not fit the current platform".to_string(),
            )
        })?;
        rows.try_reserve(statement_rows).map_err(|error| {
            safety::GraphError::Internal(format!(
                "search candidate allocation failed after reservation: {error}"
            ))
        })?;
        Spi::connect(|client| {
            // The per-statement reservation includes 8 KiB of fixed slack for
            // both parameter Vecs and their pgrx datum wrappers.
            let params = statement
                .params
                .iter()
                .map(|param| param.as_str().into())
                .collect::<Vec<_>>();
            let result = client
                .select(&statement.query, None, &params)
                .map_err(|e| {
                    safety::GraphError::Internal(format!(
                        "source-table search failed for {}: {}",
                        statement.table_name, e
                    ))
                })?;
            for row in result {
                governor
                    .consume_work(
                        crate::resource::ResourcePhase::QueryCandidates,
                        crate::resource::WorkUnits::new(1),
                    )
                    .map_err(crate::safety::resource_limit_error)?;
                governor
                    .check_elapsed(crate::resource::ResourcePhase::QueryCandidates)
                    .map_err(crate::safety::resource_limit_error)?;
                crate::resource::check_postgres_interrupts();
                let node_id = row
                    .get::<String>(1)
                    .map_err(|e| {
                        safety::GraphError::Internal(format!("search PK read failed: {}", e))
                    })?
                    .ok_or_else(|| {
                        safety::GraphError::Internal("search returned NULL PK".to_string())
                    })?;
                let Some(actual) = row.get::<String>(2).map_err(|e| {
                    safety::GraphError::Internal(format!("search value read failed: {}", e))
                })?
                else {
                    continue;
                };
                if !matcher.matches(&actual) {
                    continue;
                }
                let node = row.get::<pgrx::JsonB>(3).map_err(|e| {
                    safety::GraphError::Internal(format!("search hydration read failed: {}", e))
                })?;
                rows.push((
                    pgrx::pg_sys::Oid::from_u32(statement.table_oid),
                    node_id,
                    match_type.clone(),
                    1.0,
                    true,
                    node,
                    statement.display_table_name.clone(),
                ));
            }
            Ok::<(), safety::GraphError>(())
        })?;
    }

    govern_search_blocking(governor, &rows)?;
    sort_search_rows(&mut rows);
    dedupe_search_rows(&mut rows);
    let page_len = rows.len().saturating_sub(offset).min(limit);
    let page_bytes = page_len
        .checked_mul(std::mem::size_of::<SearchOutputRow>())
        .and_then(crate::resource::ByteCount::from_usize)
        .ok_or_else(|| {
            safety::GraphError::Internal("search page workspace overflowed".to_string())
        })?;
    let page_lease = governor
        .reserve_memory(crate::resource::ResourcePhase::QueryBlocking, page_bytes)
        .map_err(crate::safety::resource_limit_error)?;
    let mut page = Vec::new();
    page.try_reserve_exact(page_len).map_err(|error| {
        safety::GraphError::Internal(format!(
            "search page allocation failed after reservation: {error}"
        ))
    })?;
    page.extend(rows.into_iter().skip(offset).take(limit));
    workspace.retain_until_governor_drop();
    page_lease.retain_until_governor_drop();
    Ok(page)
}

fn search_matcher_workspace_bound(
    property_value: &str,
    mode: types::SearchMode,
    case_sensitive: bool,
) -> safety::GraphResult<crate::resource::ByteCount> {
    let comparable_bytes = property_value
        .len()
        .checked_mul(if case_sensitive { 1 } else { 12 });
    let token_slots = if mode == types::SearchMode::Token {
        property_value
            .split(|ch: char| !ch.is_alphanumeric())
            .filter(|token| !token.is_empty())
            .count()
            .max(1)
            .checked_mul(if case_sensitive { 1 } else { 3 })
    } else {
        Some(0)
    };
    let token_headers = token_slots
        .and_then(|tokens| tokens.checked_mul(std::mem::size_of::<String>()))
        .and_then(|bytes| bytes.checked_mul(2));
    let bytes = comparable_bytes
        .and_then(|bytes| bytes.checked_mul(3))
        .and_then(|bytes| bytes.checked_add(token_headers?))
        .and_then(|bytes| bytes.checked_add(1_024))
        .and_then(crate::resource::ByteCount::from_usize)
        .ok_or_else(|| {
            safety::GraphError::Internal("search matcher workspace overflowed".to_string())
        })?;
    Ok(bytes)
}

#[derive(Debug, Clone, Copy)]
struct SearchMaterializationSizes {
    rows: u64,
    id_bytes: u64,
    actual_bytes: u64,
    max_actual_bytes: u64,
    json_binary_bytes: u64,
    json_text_bytes: u64,
}

fn search_statement_sizes(
    statement: &SourceSearchStatement,
) -> safety::GraphResult<SearchMaterializationSizes> {
    Spi::connect(|client| {
        let params = statement
            .params
            .iter()
            .map(|param| param.as_str().into())
            .collect::<Vec<_>>();
        let result = client
            .select(&statement.size_query, None, &params)
            .map_err(|error| {
                safety::GraphError::Internal(format!(
                    "source-table search size preflight failed for {}: {error}",
                    statement.table_name
                ))
            })?;
        let row = result.first();
        let read = |column, label: &str| -> safety::GraphResult<u64> {
            let value = row.get::<i64>(column).map_err(|error| {
                safety::GraphError::Internal(format!("search {label} size read failed: {error}"))
            })?;
            Ok(u64::try_from(value.unwrap_or(0).max(0)).unwrap_or(0))
        };
        Ok(SearchMaterializationSizes {
            rows: read(1, "row count")?,
            id_bytes: read(2, "primary-key")?,
            actual_bytes: read(3, "property value")?,
            max_actual_bytes: read(4, "maximum property value")?,
            json_binary_bytes: read(5, "JSONB")?,
            json_text_bytes: read(6, "JSON text")?,
        })
    })
}

fn reserve_search_materialization(
    workspace: &mut crate::resource::ResourceLease<'_>,
    sizes: SearchMaterializationSizes,
    display_name_bytes: usize,
    match_type_bytes: usize,
) -> safety::GraphResult<()> {
    let row_overhead = u64::try_from(std::mem::size_of::<SearchOutputRow>())
        .unwrap_or(u64::MAX)
        .checked_add(u64::try_from(display_name_bytes).unwrap_or(u64::MAX))
        .and_then(|bytes| bytes.checked_add(u64::try_from(match_type_bytes).unwrap_or(u64::MAX)))
        .and_then(|bytes| bytes.checked_add(256))
        .ok_or_else(|| {
            safety::GraphError::Internal("search row estimate overflowed".to_string())
        })?;
    let bytes = sizes
        .rows
        .checked_mul(row_overhead)
        .and_then(|bytes| bytes.checked_add(sizes.id_bytes))
        .and_then(|bytes| bytes.checked_add(sizes.actual_bytes.checked_mul(4)?))
        .and_then(|bytes| bytes.checked_add(sizes.max_actual_bytes.checked_mul(4)?))
        .ok_or_else(|| {
            safety::GraphError::Internal("search materialization estimate overflowed".to_string())
        })?;
    workspace
        .try_grow(crate::resource::ByteCount::from_bytes(bytes))
        .map_err(crate::safety::resource_limit_error)?;
    crate::sql_hydration::reserve_jsonb_materialization(
        workspace,
        sizes.rows,
        sizes.json_binary_bytes,
        sizes.json_text_bytes,
    )
}

fn govern_search_blocking(
    governor: &crate::resource::ResourceGovernor,
    rows: &[SearchOutputRow],
) -> safety::GraphResult<()> {
    let sort_bytes = rows
        .len()
        .checked_mul(std::mem::size_of::<SearchOutputRow>())
        .and_then(crate::resource::ByteCount::from_usize)
        .ok_or_else(|| {
            safety::GraphError::Internal("search sort estimate overflowed".to_string())
        })?;
    let _sort_workspace = governor
        .reserve_memory(crate::resource::ResourcePhase::QueryBlocking, sort_bytes)
        .map_err(crate::safety::resource_limit_error)?;
    let comparisons = if rows.len() < 2 {
        rows.len()
    } else {
        rows.len()
            .checked_mul(usize::BITS as usize - rows.len().leading_zeros() as usize)
            .ok_or_else(|| {
                safety::GraphError::Internal("search sort work overflowed".to_string())
            })?
    };
    let work = comparisons.checked_add(rows.len()).ok_or_else(|| {
        safety::GraphError::Internal("search blocking work overflowed".to_string())
    })?;
    governor
        .consume_work(
            crate::resource::ResourcePhase::QueryBlocking,
            crate::resource::WorkUnits::new(u64::try_from(work).unwrap_or(u64::MAX)),
        )
        .map_err(crate::safety::resource_limit_error)?;
    governor
        .check_elapsed(crate::resource::ResourcePhase::QueryBlocking)
        .map_err(crate::safety::resource_limit_error)?;
    crate::resource::check_postgres_interrupts();
    Ok(())
}

fn sort_search_rows(rows: &mut [SearchOutputRow]) {
    rows.sort_by(|left, right| {
        left.0
            .to_u32()
            .cmp(&right.0.to_u32())
            .then_with(|| left.1.cmp(&right.1))
    });
}

fn dedupe_search_rows(rows: &mut Vec<SearchOutputRow>) {
    rows.dedup_by(|left, right| left.0 == right.0 && left.1 == right.1);
}

fn search_sql_predicate(
    value_expr: &str,
    property_value: &str,
    mode: types::SearchMode,
    case_sensitive: bool,
    first_param: usize,
) -> (String, Vec<String>) {
    let comparable_expr = if case_sensitive {
        value_expr.to_string()
    } else {
        format!("lower({})", value_expr)
    };
    let comparable_value = if case_sensitive {
        property_value.to_string()
    } else {
        property_value.to_lowercase()
    };

    match mode {
        types::SearchMode::Exact => (
            format!("{} = ${}", comparable_expr, first_param),
            vec![comparable_value],
        ),
        types::SearchMode::Prefix => {
            let pattern = format!("{}%", escape_like_pattern(&comparable_value));
            (
                format!("{} LIKE ${} ESCAPE '\\'", comparable_expr, first_param),
                vec![pattern],
            )
        }
        types::SearchMode::Contains => {
            let pattern = format!("%{}%", escape_like_pattern(&comparable_value));
            (
                format!("{} LIKE ${} ESCAPE '\\'", comparable_expr, first_param),
                vec![pattern],
            )
        }
        types::SearchMode::Token => {
            let tokens = comparable_value
                .split(|ch: char| !ch.is_alphanumeric())
                .filter(|token| !token.is_empty())
                .collect::<Vec<_>>();
            let predicates = tokens
                .iter()
                .enumerate()
                .map(|(idx, _)| format!("{} ~ ${}", comparable_expr, first_param + idx))
                .collect::<Vec<_>>();
            if predicates.is_empty() {
                ("TRUE".to_string(), Vec::new())
            } else {
                (
                    predicates.join(" AND "),
                    tokens
                        .into_iter()
                        .map(|token| {
                            format!(
                                "(^|[^[:alnum:]]){}([^[:alnum:]]|$)",
                                escape_regex_pattern(token)
                            )
                        })
                        .collect(),
                )
            }
        }
    }
}

fn escape_like_pattern(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn escape_regex_pattern(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
fn search_value_matches(
    actual: &str,
    expected: &str,
    mode: types::SearchMode,
    case_sensitive: bool,
) -> bool {
    SearchValueMatcher::new(expected, mode, case_sensitive).matches(actual)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SearchMode;
    use std::time::Duration;

    #[test]
    fn search_value_matcher_preserves_text_modes_and_case_handling() {
        assert!(search_value_matches(
            "Alpha Source",
            "alpha",
            SearchMode::Contains,
            false
        ));
        assert!(!search_value_matches(
            "Alpha Source",
            "alpha",
            SearchMode::Contains,
            true
        ));
        assert!(search_value_matches(
            "Alpha Source",
            "Alpha",
            SearchMode::Prefix,
            true
        ));
        assert!(search_value_matches(
            "Alpha Source",
            "alpha source",
            SearchMode::Exact,
            false
        ));
    }

    #[test]
    fn search_value_matcher_preserves_token_semantics() {
        assert!(search_value_matches(
            "Alpha-Source Match",
            "source alpha",
            SearchMode::Token,
            false
        ));
        assert!(!search_value_matches(
            "Alpha-Source Match",
            "source beta",
            SearchMode::Token,
            false
        ));
        assert!(search_value_matches(
            "Alpha Source",
            "",
            SearchMode::Token,
            false
        ));
    }

    #[test]
    fn high_token_count_statement_is_rejected_before_sql_construction() {
        let value = std::iter::repeat_n("a", 4_096)
            .collect::<Vec<_>>()
            .join("-");
        let bound = search_statement_workspace_bound(
            "name",
            &value,
            None,
            SearchMode::Token,
            &["id".to_string()],
            None,
        )
        .expect("checked token bound fits");
        let governor = crate::resource::ResourceGovernor::new(
            crate::resource::ResourceLimits::memory_only(crate::resource::MemoryBudget::new(
                crate::resource::ByteCount::from_mib(1).expect("test budget fits"),
            )),
        );
        let mut workspace = governor
            .reserve_memory(
                crate::resource::ResourcePhase::QueryCandidates,
                crate::resource::ByteCount::ZERO,
            )
            .expect("empty reservation succeeds");

        let error = workspace
            .try_grow(bound)
            .expect_err("high-token SQL must exceed the small budget");
        assert_eq!(error.kind(), crate::resource::ResourceKind::Memory);
        assert_eq!(
            error.phase(),
            crate::resource::ResourcePhase::QueryCandidates
        );
    }

    #[test]
    fn dense_token_matcher_accounts_for_string_headers_without_statements() {
        let value = std::iter::repeat_n("a", 4_096)
            .collect::<Vec<_>>()
            .join("-");
        let bound = search_matcher_workspace_bound(&value, SearchMode::Token, true)
            .expect("dense token matcher bound fits");
        let old_estimate = value.len() * 8 + 1_024;

        assert!(bound.as_u64() > old_estimate as u64);
        let governor = crate::resource::ResourceGovernor::new(
            crate::resource::ResourceLimits::memory_only(crate::resource::MemoryBudget::new(
                crate::resource::ByteCount::from_bytes(old_estimate as u64),
            )),
        );
        let mut workspace = governor
            .reserve_memory(
                crate::resource::ResourcePhase::QueryCandidates,
                crate::resource::ByteCount::ZERO,
            )
            .expect("empty reservation succeeds");
        assert!(workspace.try_grow(bound).is_err());
    }

    #[test]
    fn unicode_casefold_matcher_uses_expansion_and_token_headroom() {
        let value = "İ-Σ-K-ﬃ".repeat(1_024);
        let sensitive = search_matcher_workspace_bound(&value, SearchMode::Token, true)
            .expect("case-sensitive matcher bound fits");
        let insensitive = search_matcher_workspace_bound(&value, SearchMode::Token, false)
            .expect("case-insensitive matcher bound fits");

        assert!(insensitive > sensitive);
        assert!(insensitive.as_u64() > value.len() as u64 * 12);
    }

    fn test_limits(
        memory: crate::resource::ByteCount,
        work: u64,
        elapsed: Duration,
    ) -> crate::resource::ResourceLimits {
        crate::resource::ResourceLimits::bounded(
            crate::resource::MemoryBudget::new(memory),
            crate::resource::DiskBudget::UNLIMITED,
            crate::resource::RowCount::UNLIMITED,
            crate::resource::WorkUnits::new(work),
            crate::resource::ElapsedBudget::new(elapsed),
        )
    }

    fn search_row(id: &str) -> SearchOutputRow {
        (
            pgrx::pg_sys::Oid::from_u32(42),
            id.to_string(),
            "exact".to_string(),
            1.0,
            true,
            None,
            "public.nodes".to_string(),
        )
    }

    #[test]
    fn consecutive_search_materializations_share_retained_memory() {
        let sizes = SearchMaterializationSizes {
            rows: 2,
            id_bytes: 32,
            actual_bytes: 64,
            max_actual_bytes: 32,
            json_binary_bytes: 0,
            json_text_bytes: 0,
        };
        let sizing = crate::resource::ResourceGovernor::new(
            crate::resource::ResourceLimits::memory_only(crate::resource::MemoryBudget::new(
                crate::resource::ByteCount::from_mib(1).expect("test budget fits"),
            )),
        );
        let mut sizing_lease = sizing
            .reserve_memory(
                crate::resource::ResourcePhase::QueryCandidates,
                crate::resource::ByteCount::ZERO,
            )
            .expect("empty reservation succeeds");
        reserve_search_materialization(&mut sizing_lease, sizes, 12, 5)
            .expect("sizing reservation succeeds");
        let exact = sizing.memory_used();

        let shared = crate::resource::ResourceGovernor::new(
            crate::resource::ResourceLimits::memory_only(crate::resource::MemoryBudget::new(exact)),
        );
        let mut shared_lease = shared
            .reserve_memory(
                crate::resource::ResourcePhase::QueryCandidates,
                crate::resource::ByteCount::ZERO,
            )
            .expect("empty reservation succeeds");
        reserve_search_materialization(&mut shared_lease, sizes, 12, 5)
            .expect("first source search fits");
        let error = reserve_search_materialization(&mut shared_lease, sizes, 12, 5)
            .expect_err("second source search cannot reuse retained output memory");
        assert!(matches!(error, safety::GraphError::ResourceLimit { .. }));
    }

    #[test]
    fn search_blocking_work_is_cumulative() {
        let rows = vec![search_row("a"), search_row("b")];
        let governor = crate::resource::ResourceGovernor::new(test_limits(
            crate::resource::ByteCount::from_mib(1).expect("test budget fits"),
            10,
            Duration::MAX,
        ));
        govern_search_blocking(&governor, &rows).expect("first blocking phase fits");
        let error = govern_search_blocking(&governor, &rows)
            .expect_err("second blocking phase observes work already consumed");
        assert!(matches!(error, safety::GraphError::ResourceLimit { .. }));
    }

    #[test]
    fn search_blocking_uses_the_original_elapsed_clock() {
        let expired = crate::resource::ResourceGovernor::new(test_limits(
            crate::resource::ByteCount::from_mib(1).expect("test budget fits"),
            u64::MAX,
            Duration::from_millis(10),
        ));
        std::thread::sleep(Duration::from_millis(20));
        let error = govern_search_blocking(&expired, &[])
            .expect_err("search blocking observes operation start time");
        assert!(matches!(error, safety::GraphError::ResourceLimit { .. }));

        let fresh = crate::resource::ResourceGovernor::new(test_limits(
            crate::resource::ByteCount::from_mib(1).expect("test budget fits"),
            u64::MAX,
            Duration::from_millis(10),
        ));
        govern_search_blocking(&fresh, &[]).expect("fresh operation clock has not expired");
    }
}
