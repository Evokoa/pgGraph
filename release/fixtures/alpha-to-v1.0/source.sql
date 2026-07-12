\set ON_ERROR_STOP on

CREATE TABLE IF NOT EXISTS release_fixture_people (
    id text PRIMARY KEY,
    name text NOT NULL,
    manager_id text REFERENCES release_fixture_people(id)
);

TRUNCATE release_fixture_people;
INSERT INTO release_fixture_people (id, name, manager_id) VALUES
    ('root', 'Root', NULL),
    ('alice', 'Alice', 'root'),
    ('bob', 'Bob', 'root');
