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

# 第 3 节会在仓库根下造 .preflight/（临时题库 + 打包产物），原逻辑只在跑完那一节
# 时才删。中途 Ctrl-C / 被杀就会留下未跟踪目录，`git add -A` 会把它带进提交。
# 这里统一在退出时清理；成功路径的 rm 保留（幂等）。
TMP=""
cleanup_tmp() { [ -n "${TMP:-}" ] && rm -rf "$TMP" || true; }
trap cleanup_tmp EXIT

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
# 统一查找：任意 target 布局都要认（Linux CI 是 target/<linux-triple>/release）
find_bin() {
  local name="$1" cand
  for cand in "target/release/$name" "target/$TRIPLE/release/$name"; do
    [ -f "$cand" ] && { echo "$cand"; return 0; }
  done
  for cand in target/*/release/"$name"; do
    [ -f "$cand" ] && { echo "$cand"; return 0; }
  done
  return 1
}
BUILDER="$(find_bin "exam-builder$EXE" || true)"
CLIENT="$(find_bin "exam-client$EXE" || true)"

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
  # 用 Python 生成时间窗：`date -d` 是 GNU 扩展，macOS/BSD 上不存在
  # （CI 与 Windows-MSYS 恰好都是 GNU date，所以这个坑只在维护者本机暴露）。
  # 用 timezone.utc 而不是 datetime.UTC：本脚本不假设 Python 3.11。
  NS=$(python -c "import datetime;print((datetime.datetime.now(datetime.timezone.utc)-datetime.timedelta(hours=1)).strftime('%Y-%m-%dT%H:%M:%SZ'))")
  NE=$(python -c "import datetime;print((datetime.datetime.now(datetime.timezone.utc)+datetime.timedelta(hours=1)).strftime('%Y-%m-%dT%H:%M:%SZ'))")
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

# ---------------------------------------------------------------- 6. Release 说明

head_ "6. Release 说明只包含当前版本"
if [ -f "$ROOT/VERSION" ] && [ -f "$ROOT/docs/release-notes.md" ]; then
  BODY="$(PYTHONIOENCODING=utf-8 python "$ROOT/scripts/release_body.py" 2>/dev/null || true)"
  VER="$(tr -d '[:space:]' < "$ROOT/VERSION")"
  if [ -z "$BODY" ]; then
    bad "无法提取当前版本 Release 说明"
  elif printf '%s
' "$BODY" | grep -q "## 更新摘要（v${VER}）"        && [ "$(printf '%s
' "$BODY" | grep -c '^## 更新摘要（v')" = "1" ]; then
    ok "Release 说明仅包含 v${VER}"
  else
    bad "Release 说明包含历史版本或缺少当前版本"
  fi
else
  bad "缺少 VERSION 或 release-notes.md"
fi

# ---------------------------------------------------------------- 6b. 版本三处一致

head_ "6b. VERSION / Cargo.toml / pyproject.toml 版本一致"
V=$(tr -d '[:space:]' < "$ROOT/VERSION" 2>/dev/null || true)
CV=$(grep -m1 '^version = ' "$ROOT/Cargo.toml" 2>/dev/null | cut -d'"' -f2 || true)
PV=$(grep -m1 '^version = ' "$ROOT/admin/pyproject.toml" 2>/dev/null | cut -d'"' -f2 || true)
if [ -n "$V" ] && [ "$V" = "$CV" ] && [ "$V" = "$PV" ]; then
  ok "三处版本一致：$V"
else
  bad "版本不一致：VERSION=$V Cargo=$CV pyproject=$PV"
fi

# ---------------------------------------------------------------- 6c. 版本标记只落在标记位

head_ "6c. 版本标记一致（bump_version --check）"
# 发版时「全库替换版本号」会误改文档里的历史叙述（`vX.Y.Z 起`、历史发版小节、
# 审阅轮次标题）。这里机械检查所有**标记位**是否都等于 VERSION，漏改一处就红。
mkdir -p "$ROOT/target"
VMARK_LOG="$ROOT/target/version-markers.log"
if PYTHONIOENCODING=utf-8 python "$ROOT/scripts/bump_version.py" --check >"$VMARK_LOG" 2>&1; then
  ok "版本标记一致（历史叙述未被误改）"
else
  bad "版本标记不一致：$(tail -3 "$VMARK_LOG" | tr '\n' ' ')"
fi
rm -f "$VMARK_LOG"

# ---------------------------------------------------------------- 7. 许可证合规

head_ "7. 许可证合规（Apache-2.0 分发 + PySide6 LGPL-3.0）"
for f in LICENSE THIRD-PARTY-NOTICES.md licenses/LGPL-3.0.txt licenses/GPL-3.0.txt; do
  if [ -f "$ROOT/$f" ]; then ok "存在 $f"; else bad "缺少 $f（发布包必须带上）"; fi
done

# ---------------------------------------------------------------- 8. 管理端测试

head_ "8. 管理端测试"
if command -v uv >/dev/null 2>&1 && [ -d "$ROOT/admin" ]; then
  # 不用 /tmp：Windows 的 MSYS 环境可能没有 /tmp（实测会直接 no such file），
  # 写到仓库 target/ 下更稳，也不会污染系统临时目录。
  mkdir -p "$ROOT/target"
  PYTEST_LOG="$ROOT/target/preflight-pytest.log"
  if (cd "$ROOT" && bash scripts/run-admin-tests.sh >"$PYTEST_LOG" 2>&1); then
    ok "pytest 全部通过"
  else
    bad "pytest 失败：$(tail -3 "$PYTEST_LOG" | tr '\n' ' ')"
  fi
  rm -f "$PYTEST_LOG"
else
  echo "  （跳过：没有 uv/admin）"
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
