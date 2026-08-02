#!/usr/bin/env bash

pggraph_venv_pip() {
  local venv_dir="$1"
  shift

  if command -v sfw >/dev/null 2>&1; then
    PATH="${venv_dir}/bin:${PATH}" sfw pip "$@"
    return
  fi

  echo "sfw is not available; using the sandbox virtualenv's pip directly." >&2
  "${venv_dir}/bin/python" -m pip "$@"
}
