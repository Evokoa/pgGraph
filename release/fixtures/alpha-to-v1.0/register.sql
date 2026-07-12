\set ON_ERROR_STOP on

SELECT graph.reset();
SELECT graph.add_table(
    'release_fixture_people'::regclass,
    'id',
    ARRAY['name']
);
SELECT graph.add_edge(
    'release_fixture_people'::regclass,
    'manager_id',
    'release_fixture_people'::regclass,
    'id',
    'managed_by',
    false
);
SELECT * FROM graph.build();
