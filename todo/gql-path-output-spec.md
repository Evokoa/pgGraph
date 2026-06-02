# GQL Path Pattern Output Spec

## Scope

This spec defines JSON output for path pattern variables introduced by queries such as:

```sql
MATCH p=()-[]->() RETURN p
MATCH p=(s)-[r]->(e) RETURN p, s, r, e
```

The output shape must remain stable for hydrated and coordinate-only execution.

## Path Value

Returning a path variable produces an object with one top-level `_path` key:

```json
{
  "p": {
    "_path": {
      "nodes": [
        {
          "_id": {
            "table": "users",
            "id": "u1"
          },
          "id": "u1",
          "name": "Alice"
        },
        {
          "_id": {
            "table": "companies",
            "id": "c1"
          },
          "id": "c1",
          "name": "Acme"
        }
      ],
      "relationships": [
        {
          "_type": "works_at",
          "_start": {
            "table": "users",
            "id": "u1"
          },
          "_end": {
            "table": "companies",
            "id": "c1"
          }
        }
      ]
    }
  }
}
```

`_path.nodes` is ordered in query traversal order. `_path.relationships` is ordered by traversal step.

## Node Values

Hydrated node values include readable source-row properties plus `_id`:

```json
{
  "_id": {
    "table": "users",
    "id": "u1"
  },
  "id": "u1",
  "name": "Alice"
}
```

Coordinate-only node values, used when `hydrate := false`, include `_id` only:

```json
{
  "_id": {
    "table": "users",
    "id": "u1"
  }
}
```

The `table` value is the GQL node label derived from the registered source table, not a raw PostgreSQL OID.

## Relationship Values

Relationship values include type and endpoints:

```json
{
  "_type": "friend",
  "_start": {
    "table": "users",
    "id": "u1"
  },
  "_end": {
    "table": "users",
    "id": "u2"
  }
}
```

`_type` must be the actual matched relationship type. This matters for wildcard relationship patterns and dynamic label-column registrations.

`_start` and `_end` describe the registered relationship orientation. They do not flip only because a query used inbound traversal syntax.

## Path Functions

For a path variable `p`:

```sql
RETURN nodes(p) AS ns, relationships(p) AS rs, length(p) AS len
```

The output is:

```json
{
  "ns": [
    {
      "_id": {
        "table": "users",
        "id": "u1"
      }
    },
    {
      "_id": {
        "table": "companies",
        "id": "c1"
      }
    }
  ],
  "rs": [
    {
      "_type": "works_at",
      "_start": {
        "table": "users",
        "id": "u1"
      },
      "_end": {
        "table": "companies",
        "id": "c1"
      }
    }
  ],
  "len": 1
}
```

`nodes(p)` must be byte-for-byte equal to `p._path.nodes`.

`relationships(p)` must be byte-for-byte equal to `p._path.relationships`.

`length(p)` is the number of relationship steps in the path.

## Direction Semantics

For outbound syntax:

```sql
MATCH p=()-[]->() RETURN p
```

Nodes are ordered source to target in query traversal order.

For inbound syntax:

```sql
MATCH p=()<-[]-() RETURN p
```

Nodes are still ordered in query traversal order, while each relationship's `_start` and `_end` remain the registered relationship endpoints.

For undirected syntax:

```sql
MATCH p=()-[]-() RETURN p
```

The executor may return either traversal orientation for a stored edge, but it must not return duplicate rows for the same registered relationship unless the underlying graph contains distinct relationships.

## Null Semantics

Phase 1 does not support `OPTIONAL MATCH p=...`. If optional path variables are added later, unmatched path variables should project as JSON null:

```json
{
  "p": null,
  "ns": null,
  "rs": null,
  "len": null
}
```

## Phase 1 Output Contract

Phase 1 supports only path variable projection and path functions:

```sql
MATCH p=()-[]->() RETURN p
MATCH p=()-[]->() RETURN nodes(p), relationships(p), length(p)
```

No node or relationship variables are visible in Phase 1. The following should fail with a binding error:

```sql
MATCH p=()-[]->() RETURN s
MATCH p=()-[r]->() RETURN r
```

## Phase 2 Output Contract

Phase 2 adds explicit node and relationship variable projection:

```sql
MATCH p=(s)-[r]->(e) RETURN p, s, r, e
```

Expected output:

```json
{
  "p": {
    "_path": {
      "nodes": [
        {
          "_id": {
            "table": "users",
            "id": "u1"
          }
        },
        {
          "_id": {
            "table": "users",
            "id": "u2"
          }
        }
      ],
      "relationships": [
        {
          "_type": "friend",
          "_start": {
            "table": "users",
            "id": "u1"
          },
          "_end": {
            "table": "users",
            "id": "u2"
          }
        }
      ]
    }
  },
  "s": {
    "_id": {
      "table": "users",
      "id": "u1"
    }
  },
  "r": {
    "_type": "friend",
    "_start": {
      "table": "users",
      "id": "u1"
    },
    "_end": {
      "table": "users",
      "id": "u2"
    }
  },
  "e": {
    "_id": {
      "table": "users",
      "id": "u2"
    }
  }
}
```

`s` and `e` are projected in query binding order. `r` is the matched relationship value. `p._path.relationships[0]` and `r` must describe the same relationship.

## Error Cases

These cases should return typed GQL syntax or binding errors, not internal execution errors:

- Unknown path variable in `RETURN`.
- Duplicate variable names across path, node, and relationship positions.
- Property access on an unlabeled wildcard node when the property is ambiguous or not present on every possible concrete table.
- Relationship variable return when the pattern relationship was anonymous.
- Path functions with non-path arguments.
- Unsupported writes over path variables.
