SELECT count(*) FROM graph.shortest_path('public.p9_latency_nodes'::regclass, '1', 'public.p9_latency_nodes'::regclass, '33', 4, hydrate := false);
