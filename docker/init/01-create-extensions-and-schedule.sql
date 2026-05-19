CREATE EXTENSION IF NOT EXISTS pg_cron;
CREATE EXTENSION IF NOT EXISTS graph;

SELECT cron.schedule(
  'pggraph-maintenance',
  '*/5 * * * *',
  $$SELECT * FROM graph.run_scheduled_maintenance();$$
)
WHERE NOT EXISTS (
  SELECT 1
  FROM cron.job
  WHERE jobname = 'pggraph-maintenance'
);
