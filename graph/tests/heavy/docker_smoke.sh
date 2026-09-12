#!/usr/bin/env bash
set -euo pipefail

IMAGE="${IMAGE:-pggraph:smoke}"
CONTAINER="${CONTAINER:-pggraph-smoke}"
PG_PORT="${PG_PORT:-55432}"
PG_MAJOR="${PG_MAJOR:-17}"
POSTGRES_PASSWORD="${POSTGRES_PASSWORD:-postgres}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

if [[ ! "$PG_PORT" =~ ^[0-9]{1,5}$ ]] || (( 10#$PG_PORT < 1 || 10#$PG_PORT > 65535 )); then
  echo "PG_PORT must be between 1 and 65535" >&2
  exit 2
fi
if docker container inspect "$CONTAINER" >/dev/null 2>&1; then
  echo "Container already exists: $CONTAINER. Choose a fresh CONTAINER name." >&2
  exit 2
fi

docker build \
  --build-arg "PG_MAJOR=${PG_MAJOR}" \
  --build-arg "POSTGRES_IMAGE=postgres:${PG_MAJOR}-bookworm" \
  -t "$IMAGE" \
  "$ROOT_DIR"
container_id="$(docker run -d --name "$CONTAINER" -e "POSTGRES_PASSWORD=${POSTGRES_PASSWORD}" -p "127.0.0.1:${PG_PORT}:5432" "$IMAGE")"

cleanup() {
  docker rm -f "$container_id" >/dev/null 2>&1 || true
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
SQL

actual="$(docker exec -i "$container_id" psql -X -qAt -U postgres -v ON_ERROR_STOP=1 <<'SQL'
SELECT count(*) || ':' || coalesce(string_agg(node_id, ',' ORDER BY node_id), '')
FROM graph.search('name', 'Child', table_filter := 'graph_docker_nodes'::regclass);
SELECT string_agg(node_id, ',' ORDER BY depth)
FROM graph.traverse('graph_docker_nodes'::regclass, 'child', 1);
SQL
)"
if [[ "$actual" != $'1:child\nchild,root' ]]; then
  printf 'Docker smoke result mismatch. Expected search 1:child and traversal child,root; got:\n%s\n' "$actual" >&2
  exit 1
fi

echo "Docker smoke passed for image: $IMAGE"
