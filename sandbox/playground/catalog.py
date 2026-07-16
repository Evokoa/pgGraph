"""Structured query catalog shared by the playground UI and release gate."""

from __future__ import annotations

import re
import hashlib
from dataclasses import dataclass

from queries import DEFAULT_QUESTION, QUERY_QUESTIONS, query_sections


@dataclass(frozen=True)
class QueryExample:
    """One stable, bounded playground example."""

    id: str
    title: str
    question: str
    section: str
    modes: tuple[str, ...]
    capabilities: tuple[str, ...]
    statements: tuple[str, ...]
    expected_schema: tuple[str, ...] | None
    statement_checksum: str
    documentation_url: str
    owner: str
    minimum_pggraph_version: str
    postgresql_major: int
    reset_strategy: str
    smoke_test: str

    @property
    def sql(self) -> str:
        """Return display SQL while retaining explicit execution boundaries."""
        return ";\n\n".join(self.statements) + ";"


def _stable_id(title: str) -> str:
    return re.sub(r"[^a-z0-9]+", "-", title.lower()).strip("-")


def _statements(title: str, sql: str) -> tuple[str, ...]:
    if title == "Status + Catalog":
        return (
            "SELECT * FROM graph.status()",
            "SELECT * FROM graph.registered_tables()",
            "SELECT * FROM graph.registered_edges()",
        )
    if title == "Component Stats":
        return (
            "SELECT * FROM graph.component_stats()",
            "SELECT * FROM graph.components(max_rows := 20)",
        )
    return (sql.rstrip().removesuffix(";"),)


def query_examples(mode: str = "csr") -> tuple[QueryExample, ...]:
    """Return examples for a normalized playground mode in display order."""
    examples: list[QueryExample] = []
    for section, queries in query_sections(mode):
        for title, sql in queries.items():
            statements = _statements(title, sql)
            modes = ("mutable",) if section == "Mutable GQL Writes" else ("csr", "mutable")
            capabilities = ("gql-write",) if section == "Mutable GQL Writes" else ("gql",) if title.startswith("GQL ") else ("sql-api",)
            examples.append(
                QueryExample(
                    id=_stable_id(title),
                    title=title,
                    question=QUERY_QUESTIONS.get(title, DEFAULT_QUESTION),
                    section=section,
                    modes=modes,
                    capabilities=capabilities,
                    statements=statements,
                    expected_schema=None,
                    statement_checksum=hashlib.sha256("\0".join(statements).encode("utf-8")).hexdigest(),
                    documentation_url="https://docs.evokoa.com/pggraph/user_guide/api-reference",
                    owner="pgGraph maintainers",
                    minimum_pggraph_version="1.0.0",
                    postgresql_major=17,
                    reset_strategy="Recreate the disposable playground database for a clean catalog.",
                    smoke_test="graph/tests/heavy/playground_release_gate.sh",
                )
            )
    return tuple(examples)


def query_catalog(mode: str = "csr") -> dict[str, QueryExample]:
    """Index the shared catalog by stable display title."""
    return {example.title: example for example in query_examples(mode)}
