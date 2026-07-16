import base64
import json
from pathlib import Path

import streamlit as st

from bootstrap import ensure_graph_loaded
from catalog import query_examples
from client import DatabaseClient
from config import PlaygroundConfig
from execution import run_with_error_handling
from queries import (
    DEFAULT_QUESTION,
    DEFAULT_SQL,
    PLAYGROUND_CONTEXT,
)


@st.cache_resource(show_spinner=False)
def runtime() -> tuple[PlaygroundConfig, DatabaseClient]:
    config = PlaygroundConfig.from_environment()
    return config, DatabaseClient(config)


def asset_path(name: str) -> Path:
    config, _ = runtime()
    return config.assets_dir / name


def svg_data_uri(path: Path) -> str:
    if not path.exists():
        return ""
    payload = base64.b64encode(path.read_bytes()).decode("ascii")
    return f"data:image/svg+xml;base64,{payload}"


def fetch_one(conn, sql: str) -> dict:
    with conn.cursor() as cur:
        cur.execute(sql)
        row = cur.fetchone()
        return dict(row) if row else {}


def render_result(result: dict) -> None:
    if not result:
        st.code("Run a query to see results.", language="json")
        return

    elapsed = result.get("elapsed", "unknown")
    if result.get("ok"):
        st.caption(f"Completed in {elapsed}")
    else:
        st.caption(f"Failed after {elapsed}")
        st.error(result.get("error", "Query failed."))
        return

    result_sets = result.get("result_sets", [])
    messages = result.get("messages", [])
    view = st.radio("Result view", ["Table", "Raw JSON"], horizontal=True, label_visibility="collapsed")

    if view == "Raw JSON":
        st.code(json.dumps(result_sets or messages, indent=2, default=str), language="json")
        return

    if not result_sets:
        st.info("\n".join(messages))
        return

    for result_set in result_sets:
        rows = result_set["rows"]
        label = f"Result {result_set['index']} - {result_set['row_count']:,} rows"
        st.caption(label)
        if rows:
            st.dataframe(rows, use_container_width=True, hide_index=True)
        else:
            st.info("No rows returned.")


def render_loading_button(slot) -> None:
    slot.markdown(
        """
        <div class="loading-button">
          <span class="loading-spinner"></span>
          <span>Running SQL...</span>
        </div>
        """,
        unsafe_allow_html=True,
    )


def render_main_top() -> None:
    st.markdown(
        """
        <div class="main-top">
          <div></div>
          <div class="dataset-label">ICIJ Offshore Leaks</div>
        </div>
        """,
        unsafe_allow_html=True,
    )


def render_metric_strip(status: dict | None) -> None:
    status = status or {}
    st.markdown(
        f"""
        <div class="metric-strip">
          <div class="metric"><div class="metric-label">Nodes</div><div class="metric-value">{status.get("node_count", 0):,}</div></div>
          <div class="metric"><div class="metric-label">Edges</div><div class="metric-value">{status.get("edge_count", 0):,}</div></div>
          <div class="metric"><div class="metric-label">Schema</div><div class="metric-value">{status.get("schema_status", "loading")}</div></div>
          <div class="metric"><div class="metric-label">Sync</div><div class="metric-value">{status.get("sync_status", "loading")}</div></div>
        </div>
        """,
        unsafe_allow_html=True,
    )


def initialize_graph() -> tuple[object, dict]:
    with st.status("Preparing Panama graph...", expanded=True) as status_box:
        st.write("Connecting to PostgreSQL and checking the loaded dataset.")
        config, client = runtime()
        conn = client.connection()
        ensure_graph_loaded(conn, config)
        st.write("Verifying graph catalog registration and build status.")
        graph_status = fetch_one(conn, "SELECT * FROM graph.status();")
        status_box.update(label="Panama graph is ready.", state="complete", expanded=False)
    return conn, graph_status


def apply_css() -> None:
    st.markdown(
        """
        <style>
        :root {
          --bg: #050505;
          --panel: #1d1d1f;
          --panel-2: #111113;
          --line: #303035;
          --text: #f4f4f5;
          --muted: #8d8d95;
          --accent: #f7f7f8;
        }
        .stApp {
          background: var(--bg);
          color: var(--text);
        }
        header, [data-testid="stToolbar"], [data-testid="stDecoration"] {
          display: none !important;
        }
        section[data-testid="stSidebar"] {
          width: 366px !important;
          background: var(--panel);
          border-right: 1px solid #2b2b30;
        }
        section[data-testid="stSidebar"] > div {
          padding: 24px 24px 32px;
        }
        .main .block-container {
          max-width: 1296px;
          padding: 42px 64px 54px;
        }
        .brand-row {
          display: flex;
          align-items: center;
          gap: 12px;
          padding-bottom: 28px;
          border-bottom: 1px solid var(--line);
          margin-bottom: 38px;
        }
        .brand-row img {
          height: 28px;
          width: 28px;
        }
        .brand-name {
          font-weight: 700;
          font-size: 17px;
          color: var(--text);
        }
        .brand-product {
          color: var(--muted);
          font-size: 16px;
        }
        .main-top {
          display: flex;
          justify-content: space-between;
          align-items: center;
          border-bottom: 1px solid #1f1f22;
          padding-bottom: 22px;
          margin-bottom: 28px;
        }
        .dataset-label {
          color: var(--muted);
          font-size: 14px;
        }
        .metric-strip {
          display: grid;
          grid-template-columns: repeat(4, minmax(0, 1fr));
          gap: 12px;
          margin-bottom: 22px;
        }
        .metric {
          background: var(--panel-2);
          border: 1px solid #222228;
          border-radius: 8px;
          padding: 14px 16px;
        }
        .metric-label {
          color: var(--muted);
          font-size: 12px;
          margin-bottom: 6px;
        }
        .metric-value {
          color: var(--text);
          font-size: 20px;
          font-weight: 700;
          overflow-wrap: anywhere;
        }
        .workflow {
          color: var(--muted);
          font-size: 13px;
          margin: 18px 0 8px;
        }
        .stButton > button {
          width: 100%;
          border-radius: 8px;
          border: 1px solid #33333a;
          background: #151518;
          color: var(--text);
          text-align: left;
          padding: 9px 12px;
        }
        .stButton > button:hover {
          border-color: #56565f;
          background: #202026;
          color: #fff;
        }
        .loading-button {
          width: 100%;
          min-height: 38px;
          display: flex;
          align-items: center;
          justify-content: center;
          gap: 10px;
          border-radius: 8px;
          border: 1px solid #44444c;
          background: #202026;
          color: var(--text);
          font-size: 14px;
          font-weight: 600;
        }
        .loading-spinner {
          width: 14px;
          height: 14px;
          border: 2px solid #6f6f78;
          border-top-color: #f4f4f5;
          border-radius: 999px;
          animation: pggraph-spin 0.8s linear infinite;
        }
        @keyframes pggraph-spin {
          to { transform: rotate(360deg); }
        }
        .stTextArea textarea {
          background: #0f0f12 !important;
          color: #f4f4f5 !important;
          border: 1px solid #2b2b30 !important;
          border-radius: 8px !important;
          font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
          font-size: 13px;
        }
        .stCodeBlock pre {
          background: #0b0b0d !important;
          border: 1px solid #24242a;
          border-radius: 8px;
        }
        a {
          color: #d7d7dc !important;
        }
        </style>
        """,
        unsafe_allow_html=True,
    )


def sidebar() -> None:
    logo = svg_data_uri(asset_path("favicon.svg"))
    logo_img = f'<img src="{logo}" alt="evokoa logo" />' if logo else ""
    st.sidebar.markdown(
        f"""
        <div class="brand-row">
          {logo_img}
          <span class="brand-name">evokoa</span>
          <span class="brand-product">pgGraph Playground</span>
        </div>
        """,
        unsafe_allow_html=True,
    )
    config, _ = runtime()
    st.sidebar.caption(f"Mode: {config.build_mode}")
    filter_text = st.sidebar.text_input("Filter queries...", label_visibility="collapsed", placeholder="Filter queries...")
    normalized_filter = filter_text.strip().lower()
    examples = query_examples(config.mode)
    for section in dict.fromkeys(example.section for example in examples):
        visible_examples = [example for example in examples if example.section == section and (not normalized_filter or normalized_filter in example.title.lower() or normalized_filter in section.lower())]
        if not visible_examples:
            continue
        st.sidebar.markdown(f'<div class="workflow">{section}</div>', unsafe_allow_html=True)
        for example in visible_examples:
            if st.sidebar.button(example.title, use_container_width=True):
                st.session_state.editor_sql = example.sql
                st.session_state.catalog_sql = example.sql
                st.session_state.statements = example.statements
                st.session_state.question = example.question
                st.session_state.result = {}
    st.sidebar.divider()
    st.sidebar.link_button("Docs", "https://docs.evokoa.com/pggraph", use_container_width=True)


def main() -> None:
    st.set_page_config(
        page_title="pgGraph Playground",
        page_icon=str(asset_path("favicon.svg")),
        layout="wide",
        initial_sidebar_state="expanded",
    )
    apply_css()
    sidebar()

    if "editor_sql" not in st.session_state:
        st.session_state.editor_sql = DEFAULT_SQL
    if "statements" not in st.session_state:
        default = next(example for example in query_examples(runtime()[0].mode) if example.title == "Status + Catalog")
        st.session_state.statements = default.statements
        st.session_state.catalog_sql = default.sql
    if "question" not in st.session_state:
        st.session_state.question = DEFAULT_QUESTION
    if "result" not in st.session_state:
        st.session_state.result = {}

    render_main_top()
    metrics_slot = st.empty()
    with metrics_slot.container():
        render_metric_strip(None)

    try:
        _, status = initialize_graph()
    except Exception as exc:
        st.error(f"Could not prepare the playground graph: {type(exc).__name__}: {exc}")
        st.stop()

    with metrics_slot.container():
        render_metric_strip(status)

    st.subheader(st.session_state.question)
    st.caption(PLAYGROUND_CONTEXT)

    left, right = st.columns(2, gap="large")
    with left:
        editor_sql = st.text_area("SQL", key="editor_sql", height=430)
        run_button_slot = st.empty()
        run_clicked = run_button_slot.button("Run SQL", type="primary")
        if run_clicked:
            render_loading_button(run_button_slot)
    with right:
        if run_clicked:
            with st.spinner("Running SQL..."):
                config, client = runtime()
                connection = client.connection()
                ensure_graph_loaded(connection, config)
                statements = st.session_state.statements if editor_sql == st.session_state.catalog_sql else (editor_sql,)
                st.session_state.result = run_with_error_handling(connection, statements, config)
            st.rerun()
        render_result(st.session_state.result)


if __name__ == "__main__":
    main()
