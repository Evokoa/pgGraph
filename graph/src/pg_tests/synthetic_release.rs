#[pg_test]
fn synthetic_fixture_exercises_release_gate_sql_shape() {
    reset_and_create_synthetic_fixture(250, 60, true);

    let status = Spi::get_one::<String>(
        "SELECT node_count::text || '|' || edge_count::text
         FROM graph.status()",
    )
    .expect("synthetic status failed")
    .expect("synthetic status returned no row");
    let parts = status
        .split('|')
        .map(str::parse::<i64>)
        .collect::<Result<Vec<_>, _>>()
        .expect("synthetic status parse failed");
    assert_eq!(parts[0], 250);
    assert!(parts[1] >= 249);

    let search_rows = Spi::get_one::<i64>(
        "SELECT count(*)
         FROM graph.search(
             'name',
             'node-200',
             table_filter := 'public.graph_synth_nodes_pgtest'::regclass,
             mode := 'exact',
             max_rows := 10,
             hydrate := false
         )",
    )
    .expect("synthetic search failed")
    .unwrap_or_default();
    assert_eq!(search_rows, 1);

    let traversal_rows = Spi::get_one::<i64>(
        "SELECT count(*)
         FROM graph.traverse(
             'public.graph_synth_nodes_pgtest'::regclass,
             '1',
             3,
             direction := 'out',
             hydrate := false,
             max_rows := 10000
         )",
    )
    .expect("synthetic traverse failed")
    .unwrap_or_default();
    assert!(traversal_rows > 10);

    let filtered_rows = Spi::get_one::<i64>(
        "SELECT count(*)
         FROM graph.traverse(
             'public.graph_synth_nodes_pgtest'::regclass,
             '1',
             2,
             direction := 'out',
             filter := graph.gte('score', 500::bigint),
             hydrate := false,
             max_rows := 10000
         )",
    )
    .expect("synthetic filtered traverse failed")
    .unwrap_or_default();
    assert!(filtered_rows >= 1);

    let shortest_path_rows = Spi::get_one::<i64>(
        "SELECT count(*)
         FROM graph.shortest_path(
             'public.graph_synth_nodes_pgtest'::regclass,
             '1',
             'public.graph_synth_nodes_pgtest'::regclass,
             '200',
             20
         )",
    )
    .expect("synthetic shortest path failed")
    .unwrap_or_default();
    assert!(shortest_path_rows >= 1);

    let weighted_path_rows = Spi::get_one::<i64>(
        "SELECT count(*)
         FROM graph.weighted_shortest_path(
             'public.graph_synth_nodes_pgtest'::regclass,
             '1',
             'public.graph_synth_nodes_pgtest'::regclass,
             '200'
         )",
    )
    .expect("synthetic weighted path failed")
    .unwrap_or_default();
    assert!(weighted_path_rows >= 1);

    let component_shape = Spi::get_one::<String>(
        "SELECT num_components::text || '|' || largest_component::text
         FROM graph.component_stats()",
    )
    .expect("synthetic component stats failed")
    .expect("synthetic component stats returned no row");
    let parts = component_shape
        .split('|')
        .map(str::parse::<i64>)
        .collect::<Result<Vec<_>, _>>()
        .expect("synthetic component stats parse failed");
    assert!(parts[0] >= 1);
    assert!(parts[1] >= 125);

    let artifact_path = crate::persistence::graph_file_path().expect("graph path failed");
    assert!(
        artifact_path.exists(),
        "expected synthetic fixture to persist {}",
        artifact_path.display()
    );
}
