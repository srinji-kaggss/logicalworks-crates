#!/usr/bin/env bash
# Local gate: the documented entry point. The lane table and the checks live in
# scripts/ci_local.py and scripts/gate-lanes.toml; this wrapper keeps the
# path people already type.
#
#   ./scripts/ci-local.sh              full gate
#   ./scripts/ci-local.sh --fast       lanes marked fast
#   ./scripts/ci-local.sh --lane ID    one lane (what CI runs per step)
#   ./scripts/ci-local.sh --list       lane ids and surfaces
#   ./scripts/ci-local.sh --receipt    also write evidence/ci-local-*.md
#
# Exit 0 means every applicable lane passed on this tree. A lane whose
# prerequisite is missing is `blocked`, never a silent pass (issue #127).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export PATH="${HOME}/.cargo/bin:${PATH}"

if ! command -v python3 >/dev/null 2>&1; then
  printf 'ci-local: python3 is required and was not found on PATH\n' >&2
  exit 2
fi

exec python3 "${ROOT}/scripts/ci_local.py" "$@"
