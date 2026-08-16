"""Headless regression coverage for the playground dataframe stack."""

from __future__ import annotations

import unittest

try:
    from streamlit.testing.v1 import AppTest
except ModuleNotFoundError:
    AppTest = None


DATAFRAME_APP = """
import streamlit as st

result_sets = [
    [{"node_id": "51122", "name": "Peng Wan-Hsiung", "depth": 1}],
    [{"node_count": 2016523, "edge_count": 6678534}],
    [{"label": "officers", "countries": "Taiwan", "active": True}],
]
for rows in result_sets:
    st.dataframe(rows, width="stretch", hide_index=True)
"""


FIRST_CLICK_APP = """
from pathlib import Path
from types import SimpleNamespace

import streamlit as st

import app


class FakeClient:
    connection_generation = 1

    @staticmethod
    def connection():
        return object()


def fake_apply_css():
    st.session_state.script_runs = st.session_state.get("script_runs", 0) + 1


app.asset_path = lambda _name: Path("favicon.svg")
app.apply_css = fake_apply_css
app.sidebar = lambda: None
app.render_main_top = lambda: None
app.render_metric_strip = lambda _status: None
app.runtime = lambda: (SimpleNamespace(mode="csr"), FakeClient())
app.initialize_graph = lambda *_args: {}
app.run_with_error_handling = lambda *_args: {
    "ok": True,
    "elapsed": "44 ms",
    "result_sets": [
        {"index": 1, "row_count": 1, "rows": [{"node_count": 2016523}]},
        {"index": 2, "row_count": 1, "rows": [{"table_name": "nodes"}]},
        {"index": 3, "row_count": 1, "rows": [{"from_table": "edges"}]},
    ],
    "messages": [],
}

app.main()
"""


@unittest.skipIf(AppTest is None, "Streamlit is installed only in the playground virtualenv")
class DataframeRenderingTests(unittest.TestCase):
    """Exercise the same list-of-dicts serialization used for SQL results."""

    def test_query_result_tables_render_without_runtime_errors(self) -> None:
        assert AppTest is not None
        app = AppTest.from_string(DATAFRAME_APP)

        app.run(timeout=30)

        self.assertEqual(list(app.exception), [])
        self.assertEqual(len(app.dataframe), 3)

    def test_run_sql_renders_all_tables_with_one_script_rerun(self) -> None:
        assert AppTest is not None
        app = AppTest.from_string(FIRST_CLICK_APP)

        app.run(timeout=30)
        self.assertEqual(app.session_state.script_runs, 1)

        app.button[0].click().run(timeout=30)

        self.assertEqual(list(app.exception), [])
        self.assertEqual(app.session_state.script_runs, 2)
        self.assertEqual(len(app.dataframe), 3)
        self.assertIn("Completed in 44 ms", [caption.value for caption in app.caption])


if __name__ == "__main__":
    unittest.main()
