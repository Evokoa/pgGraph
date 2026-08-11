#!/usr/bin/env bash

pggraph_resolve_python() {
  local candidate="$1"

  "${candidate}" -c 'import os, sys; print(os.path.realpath(sys.executable))'
}

pggraph_prepare_venv_requirements() {
  local venv_dir="$1"
  local requirements="$2"
  local helper_dir
  local sfw_path
  helper_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

  if "${venv_dir}/bin/python" \
    "${helper_dir}/requirements_satisfied.py" "${requirements}" >/dev/null 2>&1 \
    && "${venv_dir}/bin/python" -m pip check >/dev/null 2>&1; then
    echo "Existing virtualenv already satisfies ${requirements}; skipping installation."
    return 0
  fi

  sfw_path="$(type -P sfw || true)"
  if [ -n "${sfw_path}" ]; then
    PATH="${venv_dir}/bin:${PATH}" "${sfw_path}" pip install -r "${requirements}"
    return
  fi

  echo "Error: sfw is required because the sandbox virtualenv does not satisfy ${requirements}." >&2
  echo "Install sfw, or provision the virtualenv from that requirements file ahead of time." >&2
  return 1
}
