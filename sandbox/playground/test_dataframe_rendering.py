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


@unittest.skipIf(AppTest is None, "Streamlit is installed only in the playground virtualenv")
class DataframeRenderingTests(unittest.TestCase):
    """Exercise the same list-of-dicts serialization used for SQL results."""

    def test_query_result_tables_render_without_runtime_errors(self) -> None:
        assert AppTest is not None
        app = AppTest.from_string(DATAFRAME_APP)

        app.run(timeout=30)

        self.assertEqual(list(app.exception), [])
        self.assertEqual(len(app.dataframe), 3)


if __name__ == "__main__":
    unittest.main()
