#!/usr/bin/env bash
set -euo pipefail

# This gate-only regression needs PostgreSQL, but no extension or large graph.
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
GATE_SQL="$SCRIPT_DIR/rls_large_table_gate.sql"
DBNAME="${DBNAME:-pggraph_rls_gate_regression_$$}"
DATABASE_CREATED=0

if [[ ! "$DBNAME" =~ ^pggraph_[A-Za-z0-9_]+$ ]] || (( ${#DBNAME} > 63 )); then
  echo "DBNAME must have a pggraph_ prefix and fit PostgreSQL's 63-byte identifier limit" >&2
  exit 2
fi

cleanup() {
  local status=$?
  trap - EXIT
  if (( DATABASE_CREATED == 1 )); then
    if ! dropdb -- "$DBNAME"; then
      echo "Could not remove this regression's database: $DBNAME" >&2
      status=1
    fi
  fi
  exit "$status"
}
trap cleanup EXIT

# Refuse an existing database rather than replacing someone else's fixture.
createdb -- "$DBNAME"
DATABASE_CREATED=1

psql -X -q -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL'
CREATE TABLE public.rls_bench_samples (
    case_name text NOT NULL,
    sample integer NOT NULL,
    result_rows bigint NOT NULL,
    result_signature text NOT NULL,
    source_rows bigint NOT NULL,
    selected_strategy text NOT NULL,
    selector_class text NOT NULL,
    relationship_completeness_checks bigint NOT NULL,
    spi_calls bigint NOT NULL,
    gql_read_recheck_calls bigint NOT NULL,
    gql_read_recheck_rows bigint NOT NULL,
    memory_peak_bytes bigint NOT NULL,
    work_units bigint NOT NULL
);
INSERT INTO public.rls_bench_samples
SELECT fixture.case_name, sample_number, fixture.result_rows, fixture.result_signature,
       fixture.source_rows, fixture.strategy, fixture.selector_class,
       fixture.completeness_checks, fixture.spi_calls, fixture.recheck_calls,
       fixture.recheck_rows, 1024, 32
FROM (VALUES
    ('p5_gql_identity_one_hop_auto', 1, 'matched', 2, 'lazy', 'targeted', 1, 3, 0, 0),
    ('p5_gql_identity_one_hop_eager_oracle', 1, 'matched', 2000, 'eager', 'targeted', 1, 3, 1, 1),
    ('p5_gql_whole_source_auto', 1000, 'whole', 2000, 'eager', 'global', 1, 3, 1, 1000),
    ('p5_no_rls_auto', 1, 'unrestricted', 0, 'lazy', 'targeted', 0, 0, 1, 1)
) AS fixture(case_name, result_rows, result_signature, source_rows, strategy,
             selector_class, completeness_checks, spi_calls, recheck_calls, recheck_rows)
CROSS JOIN generate_series(1, 2) AS sample_number;
SQL

# Two samples prove that the same file used by the runner consumes its psql
# parameter outside the dollar-quoted body rather than a hard-coded count.
psql -X -q -v ON_ERROR_STOP=1 -v samples=2 -d "$DBNAME" -f "$GATE_SQL"

psql -X -q -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL'
DELETE FROM public.rls_bench_samples
WHERE case_name = 'p5_gql_identity_one_hop_auto' AND sample = 2;
SQL

if failure=$(psql -X -q -v ON_ERROR_STOP=1 -v VERBOSITY=verbose -v samples=2 \
  -d "$DBNAME" -f "$GATE_SQL" 2>&1); then
  echo "The evidence gate accepted a missing required sample" >&2
  exit 1
fi
if [[ "$failure" != *"P0001"* \
   || "$failure" != *"P5 release profile did not retain every required sample"* ]]; then
  printf 'The evidence gate failed for an unexpected reason:\n%s\n' "$failure" >&2
  exit 1
fi

psql -X -q -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL'
INSERT INTO public.rls_bench_samples
SELECT case_name, 2, result_rows, result_signature, source_rows, selected_strategy,
       selector_class, relationship_completeness_checks, spi_calls,
       gql_read_recheck_calls, gql_read_recheck_rows, memory_peak_bytes, work_units
FROM public.rls_bench_samples
WHERE case_name = 'p5_gql_identity_one_hop_auto' AND sample = 1;
SQL
psql -X -q -v ON_ERROR_STOP=1 -v samples=2 -d "$DBNAME" -f "$GATE_SQL"

psql -X -q -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL'
UPDATE public.rls_bench_samples SET selected_strategy = 'eager'
WHERE case_name = 'p5_no_rls_auto';
SQL

if failure=$(psql -X -q -v ON_ERROR_STOP=1 -v VERBOSITY=verbose -v samples=2 \
  -d "$DBNAME" -f "$GATE_SQL" 2>&1); then
  echo "The evidence gate accepted an eager no-RLS identity-bounded route" >&2
  exit 1
fi
if [[ "$failure" != *"P0001"* \
   || "$failure" != *"P5 no-RLS auto route violated bounded strategy or zero visibility work"* ]]; then
  printf 'The evidence gate failed for an unexpected reason:\n%s\n' "$failure" >&2
  exit 1
fi

psql -X -q -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL'
UPDATE public.rls_bench_samples SET selected_strategy = 'lazy'
WHERE case_name = 'p5_no_rls_auto';
SQL
psql -X -q -v ON_ERROR_STOP=1 -v samples=2 -d "$DBNAME" -f "$GATE_SQL"

echo "Large-table RLS evidence gate accepted complete samples and rejected missing samples and an incorrect no-RLS strategy"
