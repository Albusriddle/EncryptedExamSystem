#!/usr/bin/env bash
# 发版改版本号：只改标记位，不碰历史叙述（实现见 scripts/bump_version.py）。
#
# 用法：
#   bash scripts/bump-version.sh 1.4.8          # 改到 1.4.8
#   bash scripts/bump-version.sh 1.4.8 --dry-run
#   bash scripts/bump-version.sh --check        # 发布预检 / CI 用
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# 挑一个**真的能跑**的解释器（Windows 上 python3 常被应用商店占位程序劫持）。
pick_python() {
  local cand
  for cand in python3 python; do
    if command -v "$cand" >/dev/null 2>&1 \
      && "$cand" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 11) else 1)' >/dev/null 2>&1; then
      echo "$cand"
      return 0
    fi
  done
  return 1
}

PY="$(pick_python)" || {
  echo "缺少可用的 Python 3.11+（项目要求 ≥3.11，见 admin/pyproject.toml）"
  exit 2
}

exec "$PY" "$ROOT/scripts/bump_version.py" "$@"
