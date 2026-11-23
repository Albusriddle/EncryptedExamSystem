#!/usr/bin/env bash
# 网页考试「局域网演练」：并发登录 / 作答 / 崩溃恢复 / 交卷 / 收卡校验。
#
# 用法：
#   bash scripts/drill-web.sh                     # 默认 40 人
#   bash scripts/drill-web.sh --students 50       # 50 人（考场规模上限参考值）
#   bash scripts/drill-web.sh --students 12 --no-restart   # CI 用的小规模版本
#
# 先决条件：cargo build --release -p server -p builder -p client
# 报告会打印每个阶段的 p50 / p95 / 最慢耗时，以及 .eea / 事件 / 状态文件的数量核对。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# 挑一个**真的能跑**的解释器：Windows 上 `python3` 常被解析到应用商店的占位程序，
# 它一执行就退出 49，脚本会以「python 跑不了」告终（smoke-web.sh 也踩过这个坑）。
pick_python() {
  local cand
  for cand in python3 python; do
    if command -v "$cand" >/dev/null 2>&1 \
      && "$cand" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 10) else 1)' >/dev/null 2>&1; then
      echo "$cand"
      return 0
    fi
  done
  return 1
}

PY="$(pick_python)" || {
  echo "缺少可用的 Python 3.10+（演练脚本只用标准库）"
  exit 2
}

exec "$PY" "$ROOT/scripts/drill_web.py" "$@"
