-- Shared acceptance gate for retained large-table RLS measurements.
-- Invoke with psql -v samples=N after collecting public.rls_bench_samples.
SELECT pg_catalog.set_config('pggraph_rls_evidence.required_samples', :'samples', false);

DO $p5_gate$
DECLARE
    required_samples integer := pg_catalog.current_setting('pggraph_rls_evidence.required_samples')::integer;
    paired_samples integer;
BEGIN
    IF EXISTS (
        SELECT 1
        FROM (VALUES
            ('p5_gql_identity_one_hop_auto'),
            ('p5_gql_identity_one_hop_eager_oracle'),
            ('p5_gql_whole_source_auto'),
            ('p5_no_rls_auto')
        ) AS required(case_name)
        LEFT JOIN public.rls_bench_samples AS sample USING (case_name)
        GROUP BY required.case_name
        HAVING count(sample.case_name) <> required_samples
    ) THEN
        RAISE EXCEPTION 'P5 release profile did not retain every required sample';
    END IF;
    SELECT count(*) INTO paired_samples
    FROM public.rls_bench_samples AS automatic
    JOIN public.rls_bench_samples AS oracle
      ON oracle.case_name = 'p5_gql_identity_one_hop_eager_oracle'
     AND automatic.sample = oracle.sample
    WHERE automatic.case_name = 'p5_gql_identity_one_hop_auto';
    IF paired_samples <> required_samples THEN
        RAISE EXCEPTION 'P5 targeted auto/oracle samples are not paired';
    END IF;
    IF EXISTS (
        SELECT 1
        FROM public.rls_bench_samples AS automatic
        JOIN public.rls_bench_samples AS oracle
          ON oracle.case_name = 'p5_gql_identity_one_hop_eager_oracle'
         AND automatic.sample = oracle.sample
        WHERE automatic.case_name = 'p5_gql_identity_one_hop_auto'
          AND (automatic.result_rows, automatic.result_signature)
              IS DISTINCT FROM (oracle.result_rows, oracle.result_signature)
    ) THEN
        RAISE EXCEPTION 'P5 targeted auto results differ from the eager PostgreSQL oracle';
    END IF;
    IF EXISTS (
        SELECT 1
        FROM public.rls_bench_samples AS automatic
        JOIN public.rls_bench_samples AS oracle
          ON oracle.case_name = 'p5_gql_identity_one_hop_eager_oracle'
         AND automatic.sample = oracle.sample
        WHERE automatic.case_name = 'p5_gql_identity_one_hop_auto'
          AND (automatic.source_rows > 64 OR automatic.source_rows >= oracle.source_rows)
    ) THEN
        RAISE EXCEPTION 'P5 targeted GQL source work is not bounded below the eager oracle';
    END IF;
    IF EXISTS (
        SELECT 1
        FROM public.rls_bench_samples
        WHERE case_name LIKE 'p5_%'
        GROUP BY case_name
        HAVING count(DISTINCT (result_rows, result_signature)) <> 1
    ) THEN
        RAISE EXCEPTION 'P5 exact result signature changed between samples';
    END IF;
    IF EXISTS (
        SELECT 1
        FROM public.rls_bench_samples
        WHERE case_name = 'p5_gql_identity_one_hop_auto'
          AND (selected_strategy <> 'lazy'
               OR selector_class <> 'targeted'
               OR relationship_completeness_checks <= 0)
    ) THEN
        RAISE EXCEPTION 'P5 identity-seeded GQL did not select bounded lazy visibility';
    END IF;
    IF EXISTS (
        SELECT 1
        FROM public.rls_bench_samples
        WHERE case_name = 'p5_gql_whole_source_auto'
          AND (selected_strategy <> 'eager'
               OR selector_class <> 'global'
               OR relationship_completeness_checks <= 0)
    ) THEN
        RAISE EXCEPTION 'P5 whole-source GQL did not retain the global eager oracle';
    END IF;
    -- Identity-bounded GQL uses the lazy executor without RLS probes while
    -- retaining source read rechecks for unrestricted callers.
    IF EXISTS (
        SELECT 1
        FROM public.rls_bench_samples
        WHERE case_name = 'p5_no_rls_auto'
          AND (selected_strategy <> 'lazy'
               OR selector_class <> 'targeted'
               OR spi_calls <> 0
               OR source_rows <> 0)
    ) THEN
        RAISE EXCEPTION 'P5 no-RLS auto route violated bounded strategy or zero visibility work';
    END IF;
    IF EXISTS (
        SELECT 1
        FROM public.rls_bench_samples
        WHERE case_name = 'p5_gql_identity_one_hop_auto'
          AND (gql_read_recheck_calls <> 0 OR gql_read_recheck_rows <> 0)
    ) THEN
        RAISE EXCEPTION 'P5 bounded GQL route unexpectedly repeated eager read rechecks';
    END IF;
    IF EXISTS (
        SELECT 1
        FROM public.rls_bench_samples
        WHERE case_name IN (
            'p5_gql_identity_one_hop_eager_oracle',
            'p5_gql_whole_source_auto',
            'p5_no_rls_auto'
        )
          AND (gql_read_recheck_calls <= 0 OR gql_read_recheck_rows <= 0)
    ) THEN
        RAISE EXCEPTION 'P5 eager GQL read-recheck telemetry was not recorded';
    END IF;
    IF EXISTS (
        SELECT 1
        FROM public.rls_bench_samples
        WHERE case_name LIKE 'p5_%'
          AND (memory_peak_bytes <= 0 OR work_units <= 0)
    ) THEN
        RAISE EXCEPTION 'P5 resource telemetry contains invalid values';
    END IF;
END
$p5_gate$;
