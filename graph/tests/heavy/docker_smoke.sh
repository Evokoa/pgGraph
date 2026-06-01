#!/usr/bin/env bash
set -euo pipefail

IMAGE="${IMAGE:-pggraph:smoke}"
CONTAINER="${CONTAINER:-pggraph-smoke}"
PG_MAJOR="${PG_MAJOR:-17}"
POSTGRES_PASSWORD="${POSTGRES_PASSWORD:-postgres}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

docker build \
  --build-arg "PG_MAJOR=${PG_MAJOR}" \
  --build-arg "POSTGRES_IMAGE=postgres:${PG_MAJOR}-bookworm" \
  -t "$IMAGE" \
  "$ROOT_DIR"
docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
docker run -d --name "$CONTAINER" -e "POSTGRES_PASSWORD=${POSTGRES_PASSWORD}" -p 55432:5432 "$IMAGE" >/dev/null

cleanup() {
  docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
}
trap cleanup EXIT

for _ in {1..60}; do
  if docker exec "$CONTAINER" pg_isready -U postgres >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

docker exec -i "$CONTAINER" psql -U postgres -v ON_ERROR_STOP=1 <<'SQL'
CREATE EXTENSION graph;
SELECT graph.reset();
CREATE TABLE graph_docker_nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    parent_id TEXT REFERENCES graph_docker_nodes(id)
);
INSERT INTO graph_docker_nodes VALUES ('root', 'Root', NULL), ('child', 'Child', 'root');
SELECT graph.add_table('graph_docker_nodes'::regclass, 'id', ARRAY['name']);
SELECT graph.add_edge('graph_docker_nodes'::regclass, 'parent_id', 'graph_docker_nodes'::regclass, 'id', 'parent', false);
SELECT * FROM graph.build();
SELECT count(*) FROM graph.search('name', 'Child', table_filter := 'graph_docker_nodes'::regclass);
SQL

echo "Docker smoke passed for image: $IMAGE"
