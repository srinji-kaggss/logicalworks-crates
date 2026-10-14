#!/usr/bin/env bash
# Local gate: the documented entry point. The lane table lives in
# scripts/gate-lanes.toml and the coordinator is the `ci-local` binary of
# lgwks_deps; this wrapper keeps the path people already type.
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

cd "${ROOT}"
exec cargo run --quiet --locked -p lgwks_deps --bin ci-local -- "$@"
