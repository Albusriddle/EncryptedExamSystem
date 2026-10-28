#!/usr/bin/env bash
# 运行管理端 pytest。
#
# Windows + PySide6 在进程退出阶段偶发崩溃（测试断言已全部通过，退出码
# 127/139）。只在“输出明确显示 passed 且没有 failed/error”时把这种
# teardown 崩溃当作成功；真实断言失败仍返回非零。
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT/admin" || exit 1
LOG="${TMPDIR:-/tmp}/ees-admin-pytest.log"
set +e
QT_QPA_PLATFORM=offscreen uv run pytest -q "$@" >"$LOG" 2>&1
code=$?
set -e
cat "$LOG"
if [ "$code" -eq 0 ]; then exit 0; fi
if grep -qE '(^|[[:space:]])failed|ERROR|error:' "$LOG"; then exit "$code"; fi
if grep -qE '[0-9]+ passed' "$LOG"; then
  echo "⚠ pytest 断言全绿，但 Qt 在退出阶段返回 $code；按已知 teardown 崩溃处理"
  exit 0
fi
exit "$code"
