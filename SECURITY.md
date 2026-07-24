# Security Policy

## Supported Versions

The current 1.0.x release line is the supported security target.

| Version | Supported |
|---|---|
| `1.0.x` | Yes |
| `0.x` and unreleased commits | No |

## Reporting A Vulnerability

Do not open a public issue for a suspected vulnerability.

Use GitHub's private vulnerability reporting for
`https://github.com/evokoa/pggraph` once the repository is public. If that
channel is unavailable, contact the maintainers through the private contact
method listed on the repository profile.

Please include:

- affected PostgreSQL version and pgGraph commit;
- whether the issue requires superuser, graph-admin, ordinary SQL user, or
  untrusted input access;
- a minimal reproduction when possible;
- any observed SQLSTATE, server log, crash report, or memory-safety symptom.

## Security Model

pgGraph is a PostgreSQL extension. It relies on PostgreSQL authentication,
authorization, RLS, extension installation controls, and filesystem protection
for the data directory.

The detailed security model is documented in
[Administration and Security](docs/user_guide/administration-and-security.mdx)
and [Safety and Security](docs/contributor_guide/safety-security.mdx).
