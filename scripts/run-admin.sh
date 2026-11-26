#!/usr/bin/env bash
# 从任意工作目录启动管理端，并透传 --selftest / --version / --dump-ui。
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if ! command -v uv >/dev/null; then
  echo "Missing uv. On Arch Linux: sudo pacman -S uv" >&2
  exit 1
fi
cd "$ROOT/admin"
exec uv run --locked --python 3.13 --extra gui python -m app "$@"
