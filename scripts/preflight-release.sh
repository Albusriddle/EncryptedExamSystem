#!/usr/bin/env bash
# 发布预检：在本地复现 release.yml 的每一步，避免只能靠打 tag 才发现问题。
#
# 背景：release 工作流曾经有三处只有「真实 CI 环境」才暴露的问题
# （产物在 target/<triple>/release 而非 target/release、onefile 需要可写 TEMP、
# 脚本输出编码），全部是静态审阅看不出来的。这个脚本把它们变成可重复的检查。
#
# 用法：
#   bash scripts/preflight-release.sh            # 只查 Rust 侧（快，约 1 分钟）
#   bash scripts/preflight-release.sh --full     # 连管理端打包一起查（慢，需要 PySide6）
#
# 退出码 0 表示发布链路各环节都在预期位置。

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

FULL=0
[ "${1:-}" = "--full" ] && FULL=1

PASS=0
FAIL=0

ok()   { echo "  ✓ $1"; PASS=$((PASS + 1)); }
bad()  { echo "  ✗ $1"; FAIL=$((FAIL + 1)); }
head_() { echo; echo "== $1 =="; }

# 可执行后缀探测：两种产物布局都要看，否则在"只有 triple 目录"的
# CI 布局下会把 Windows 的 .exe 当成不存在。
EXE=""
for cand in "$ROOT/target/release/exam-builder.exe" \
            "$ROOT"/target/*/release/exam-builder.exe; do
  [ -f "$cand" ] && EXE=".exe" && break
done

# ---------------------------------------------------------------- 1. 产物定位

head_ "1. 产物定位（release 工作流用 --target 构建）"
TRIPLE="x86_64-pc-windows-msvc"
for name in "exam-builder$EXE" "exam-client$EXE"; do
  found=""
  for cand in "target/release/$name" "target/$TRIPLE/release/$name"; do
    [ -f "$cand" ] && found="$cand" && break
  done
  if [ -z "$found" ]; then
    for d in target/*/release; do
      [ -f "$d/$name" ] && found="$d/$name" && break
    done
  fi
  [ -n "$found" ] && ok "$name 可在 $found 定位" || bad "$name 找不到（先 cargo build --release）"
done

# find_artifact 必须同时认两种布局
PY_SCRIPT="$ROOT/admin/buildtools/build_exe.py"
if [ -f "$PY_SCRIPT" ]; then
  if grep -q 'target" / "release"' "$PY_SCRIPT" \
     && grep -q 'x86_64-pc-windows-msvc' "$PY_SCRIPT"; then
    ok "build_exe.py 同时认 target/release 与 target/<triple>/release"
  else
    bad "build_exe.py 只认一种产物布局（CI 会失败）"
  fi
else
  bad "缺少 $PY_SCRIPT"
fi

# ---------------------------------------------------------------- 2. 冒烟

head_ "2. 冒烟测试"
BUILDER=""
for cand in "target/release/exam-builder$EXE" "target/$TRIPLE/release/exam-builder$EXE"; do
  [ -f "$cand" ] && BUILDER="$cand" && break
done
CLIENT=""
for cand in "target/release/exam-client$EXE" "target/$TRIPLE/release/exam-client$EXE"; do
  [ -f "$cand" ] && CLIENT="$cand" && break
done

if [ -n "$BUILDER" ]; then
  MASTER="$("$BUILDER" keygen 2>/dev/null)"
  [ "${#MASTER}" -eq 64 ] && ok "keygen 输出 64 位十六进制" || bad "keygen 输出长度异常"
else
  bad "找不到 exam-builder，跳过冒烟"
  MASTER=""
fi

# ---------------------------------------------------------------- 3. 端到端

head_ "3. 端到端：学生端必须零配置可解锁"
if [ -n "$BUILDER" ] && [ -n "$CLIENT" ] && [ -n "$MASTER" ]; then
  TMP="$ROOT/.preflight"
  rm -rf "$TMP"; mkdir -p "$TMP"
  NS=$(date -u -d '1 hour ago' +%Y-%m-%dT%H:%M:%SZ)
  NE=$(date -u -d '1 hour'     +%Y-%m-%dT%H:%M:%SZ)
  cat > "$TMP/m.json" <<JSON
{"version":2,"salt":"00112233445566778899aabbccddeeff",
 "sessions":[{"session_id":1,"start":"$NS","end":"$NE",
 "students":[2021001],"exams":[{"file":"demo/math.json","pick":2}]}]}
JSON
  # 注意：产物名不要用 default.exe —— 该名字在 MSYS(Windows) 下无法执行（退出码 127）
  EXAM_MASTER="$MASTER" "$BUILDER" build --manifest "$TMP/m.json" --banks banks/ \
    --stub "$CLIENT" --out "$TMP/pkg.exe" > "$TMP/build.log" 2>&1
  if [ $? -eq 0 ]; then
    grep -q "已内嵌密钥" "$TMP/build.log" && ok "默认打包已内嵌密钥" \
      || bad "默认打包未内嵌密钥（学生将无法零配置使用）"
  else
    bad "打包失败：$(tail -2 "$TMP/build.log" | tr '\n' ' ')"
  fi

  if [ -f "$TMP/pkg.exe" ]; then
    if "$TMP/pkg.exe" --student 2021001 --package "$TMP/pkg.exe" 2>/dev/null | grep -q "解锁成功"; then
      ok "学生零配置可解锁"
    else
      bad "学生零配置无法解锁（发布物不可用）"
    fi
    if "$TMP/pkg.exe" --student 9999999 --package "$TMP/pkg.exe" >/dev/null 2>&1; then
      bad "名单外学号竟然解锁成功"
    else
      ok "名单外学号被拒绝"
    fi
  fi
  rm -rf "$TMP"
else
  bad "缺少二进制，跳过端到端"
fi

# ---------------------------------------------------------------- 4. 管理端打包

head_ "4. 管理端打包（PyInstaller）"
if [ "$FULL" -eq 1 ]; then
  if [ -f "$ROOT/admin/.venv/Scripts/python.exe" ]; then
    PY="$ROOT/admin/.venv/Scripts/python.exe"
  elif [ -f "$ROOT/admin/.venv/bin/python" ]; then
    PY="$ROOT/admin/.venv/bin/python"
  else
    PY="python"
  fi
  if "$PY" -c "import PySide6, PyInstaller" 2>/dev/null; then
    ok "PySide6 与 PyInstaller 可用"
    if "$PY" "$ROOT/admin/buildtools/build_exe.py" > "$ROOT/.preflight-build.log" 2>&1; then
      ok "build_exe.py 成功产出"
      if [ -f "$ROOT/dist/ExamAdmin/ExamAdmin$EXE" ] || [ -f "$ROOT/dist/ExamAdmin/ExamAdmin.exe" ]; then
        ok "主程序位于 dist/ExamAdmin/"
      else
        bad "主程序不在预期位置"
      fi
      if [ -f "$ROOT/dist/ExamAdmin/_internal/exam-builder$EXE" ]; then
        ok "exam-builder 作为资源内嵌"
      else
        bad "exam-builder 未内嵌（教师端无法打包）"
      fi
    else
      bad "build_exe.py 失败：$(tail -3 "$ROOT/.preflight-build.log" | tr '\n' ' ')"
    fi
    rm -f "$ROOT/.preflight-build.log"
  else
    bad "缺少 PySide6/PyInstaller，无法检查管理端打包"
  fi
else
  echo "  （跳过：加 --full 启用，需要 PySide6）"
fi

# ---------------------------------------------------------------- 汇总

echo
if [ "$FAIL" -eq 0 ]; then
  echo "✓ 发布预检通过（$PASS 项）"
  exit 0
else
  echo "✗ 发布预检失败：$FAIL 项不通过（$PASS 项通过）"
  exit 1
fi
