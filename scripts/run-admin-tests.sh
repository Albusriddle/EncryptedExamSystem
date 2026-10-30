#!/usr/bin/env bash
# 运行管理端 pytest。
#
# Windows + PySide6 在进程退出阶段偶发崩溃（测试断言已全部通过，退出码
# 127/139）。这里用 JUnit XML 判断真实结果：只有 XML 证明 tests>0 且
# failures/errors 都为 0 时，才把 teardown 崩溃当作成功。
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT/admin" || exit 1
mkdir -p "$ROOT/target"
LOG="$ROOT/target/ees-admin-pytest-$RANDOM.log"
XML="$ROOT/target/ees-admin-junit-$RANDOM.xml"
rm -f "$LOG" "$XML"
set +e
QT_QPA_PLATFORM=offscreen uv run pytest -q --junitxml="$XML" "$@" >"$LOG" 2>&1
code=$?
set -e
cat "$LOG"
if [ "$code" -eq 0 ]; then exit 0; fi
if [ "$code" -ne 127 ] && [ "$code" -ne 139 ]; then exit "$code"; fi
PYTHONIOENCODING=utf-8 python - "$XML" <<'PY'
import sys
import xml.etree.ElementTree as ET

path = sys.argv[1]
try:
    root = ET.parse(path).getroot()
except Exception:
    raise SystemExit(1)
suites = root.findall("testsuite") if root.tag == "testsuites" else [root]
tests = sum(int(s.attrib.get("tests", "0")) for s in suites)
failures = sum(int(s.attrib.get("failures", "0")) for s in suites)
errors = sum(int(s.attrib.get("errors", "0")) for s in suites)
if tests > 0 and failures == 0 and errors == 0:
    print(f"⚠ pytest {tests} 项全部通过，但 Qt 退出阶段返回 127/139；按已知 teardown 崩溃处理")
    raise SystemExit(0)
print(f"pytest JUnit: tests={tests} failures={failures} errors={errors}", file=sys.stderr)
raise SystemExit(1)
PY
