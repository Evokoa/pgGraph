SELECT count(*) FROM graph.traverse('public.p9_latency_nodes'::regclass, '1', 4, hydrate := false);
