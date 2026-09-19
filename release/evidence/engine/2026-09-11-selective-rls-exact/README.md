# Selective RLS exact-result comparisons

All 46 ordered eager/automatic comparisons and two PostgreSQL backend cleanup
checks passed on source commit `5e9977151c3e2419a570e6fc78421ef6cf160df0`.
The tested library used PostgreSQL 17.11 on Linux aarch64, the optimized release
profile, and features `pg17 development`, without `pg_test`. Its SHA-256 is
`a92759fd50f1805bb0f1d15fd7dc5f444cdc53150d7cf1b51aea294384f14909`.

`results.json` retains complete emitted rows in order, immediate strategy and
resource metrics, and the cleanup/error/retry records. Comparisons cover:

- Directed and bidirectional CSR, outbound BFS and DFS in out/in/any directions, multiple roots,
  unweighted paths and supported weighted paths.
- Hidden roots, targets, intermediate nodes and relationships; workflow counts,
  resolver reuse and truncation; eligible GQL/Cypher expansions and OPTIONAL
  null-extension; intentionally eager joins and whole-source scans.
- Actual in-memory pending deltas, durable segment publication and a fresh
  connection, transaction-local edges and the eager transaction-node fallback.
- Injected cancellation (`57014`) and a policy error (`P0001`), followed by
  empty temporary visibility state and identical successful retry rows in the
  same PostgreSQL backend.

The cleanup fixture disables node RLS while retaining relationship RLS to
isolate relationship-state cleanup. Its visible node set consequently differs
from the ordinary traversal fixture. The no-RLS case checks the no-probe path.
These are small synthetic correctness cases, not scale or latency measurements.

## Reproduction

Use a fresh disposable PostgreSQL 17 cluster with `fsync=on`, a private Unix
socket, and the extension built from the recorded source with
`cargo pgrx install --release --no-default-features --features 'pg17 development'`.
Use the installed library's actual checksum in a build record with the same
fields as `build-record.json`. A new build can have a different binary checksum.
The retained run had four CPUs and a 4 GiB container limit.

Create a project Python virtual environment; the harness needs only the Python
standard library and the recorded source's `graph/tests/heavy/psql_session.py`.
Copy this evidence directory outside the source checkout before checking out
the recorded commit, then run:

```sh
PGHOST=/path/to/private-socket PGPORT=55417 PGUSER=postgres \
  /path/to/venv/bin/python /path/to/evidence/reproduce.py \
  --repository /path/to/checkout \
  --output /path/to/new-results \
  --build-record /path/to/actual-build-record.json \
  --admin-database postgres --disposable
```

The harness creates six synthetic databases and roles with a unique prefix,
refuses collisions, and retains them for inspection. It does not install the
extension or start or stop PostgreSQL. Run it with an administrative role in
the disposable cluster; graph queries execute as the fixture reader roles.
The wrapper that recorded this run verified unchanged source, library and
runner hashes and stopped its owned cluster successfully.

Measurements and the reproduction harness are retained verbatim. The local
socket path is omitted from the selected runtime facts; see
`selection-provenance.json`. Generated fixture identities and database OIDs
remain in the result rows. `SHA256SUMS` covers the selected files except itself.
