#!/usr/bin/env bash
# 外部证书自检冒烟：用 `--gen-cert` 现造两套自签证书，验证四种启动结果。
#
# 为什么需要这个脚本：自检逻辑的 6 条单元测试都在 Rust 里直接调 `check_external()`，
# 覆盖不到「CLI 参数解析 → 分级 → 退出码 → 横幅文案」这一层集成面。
# v1.6.0 发版时就是靠手工跑真二进制验证的，这里把它固化成可重复的门禁。
set -uo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
# 二进制定位：CI 的 release-preflight 用 `cargo build --release --target <triple>` 构建，
# 产物在 `target/<triple>/release`；本地是 `target/release`。两处都认。
if [ -z "${BIN:-}" ]; then
  for cand in "$ROOT/target/release" "$ROOT"/target/*/release; do
    if [ -x "$cand/exam-server" ] || [ -x "$cand/exam-server.exe" ]; then
      BIN=$cand
      break
    fi
  done
fi
BIN=${BIN:-$ROOT/target/release}
EXE=""
[ -f "$BIN/exam-server.exe" ] && EXE=".exe"
SERVER="$BIN/exam-server$EXE"
BUILDER="$BIN/exam-builder$EXE"
CLIENT="$BIN/exam-client$EXE"
PY=${PY:-python3}
command -v "$PY" >/dev/null 2>&1 || PY=python

[ -x "$SERVER" ] || { echo "缺少 $SERVER（先 cargo build --release）"; exit 1; }
[ -x "$BUILDER" ] || { echo "缺少 $BUILDER"; exit 1; }
[ -x "$CLIENT" ] || { echo "缺少 $CLIENT"; exit 1; }

WORK="$ROOT/target/tls-cert-smoke"
rm -rf "$WORK"
mkdir -p "$WORK"

FAIL=0
ok() { echo "  ✓ $1"; }
bad() {
  echo "  ✗ $1"
  FAIL=$((FAIL + 1))
}

free_port() {
  "$PY" - <<'PY'
import socket

s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
}

# ---------------------------------------------------------------- 准备考试包
MASTER=$("$BUILDER" keygen 2>/dev/null | tr -d '\r\n')
[ "${#MASTER}" -eq 64 ] || { echo "keygen 输出异常"; exit 1; }
START=$("$PY" -c "import datetime;print((datetime.datetime.now(datetime.UTC)-datetime.timedelta(minutes=30)).strftime('%Y-%m-%dT%H:%M:%SZ'))")
END=$("$PY" -c "import datetime;print((datetime.datetime.now(datetime.UTC)+datetime.timedelta(hours=2)).strftime('%Y-%m-%dT%H:%M:%SZ'))")
cat >"$WORK/live.json" <<JSON
{"version":2,"salt":"00112233445566778899aabbccddeeff","sessions":[
 {"session_id":1,"start":"$START","end":"$END","students":[2021001],
  "exams":[{"file":"demo/math.json","pick":1}]}]}
JSON
EXAM_MASTER=$MASTER "$BUILDER" build --manifest "$WORK/live.json" --banks "$ROOT/banks" \
  --stub "$CLIENT" --out "$WORK/pkg" --emit-keyfile "$WORK/key.txt" >"$WORK/build.log" 2>&1 ||
  { echo "打包失败："; tail -3 "$WORK/build.log"; exit 1; }

# ---------------------------------------------------------------- 四套证书
# A/B 两套「默认 hosts」，C 只有 10.0.0.9，D 只有 ::1
for d in A B; do
  "$SERVER" --gen-cert --answers-dir "$WORK/$d" --tls-host 127.0.0.1 >/dev/null 2>&1 ||
    { echo "生成证书 $d 失败"; exit 1; }
done
"$SERVER" --gen-cert --answers-dir "$WORK/C" --tls-host 10.0.0.9 >/dev/null 2>&1 || exit 1
"$SERVER" --gen-cert --answers-dir "$WORK/D" --tls-host ::1 >/dev/null 2>&1 || exit 1

run_expect_fail() {
  local desc=$1 log=$2 want=$3
  shift 3
  "$SERVER" "$@" >"$log" 2>&1
  local code=$?
  if [ "$code" -eq 0 ]; then
    bad "$desc：应当拒绝启动，却返回 0"
    return
  fi
  if grep -q -- "$want" "$log"; then
    ok "$desc：拒绝启动且提示含「$want」"
  else
    bad "$desc：拒绝启动但提示里没有「$want」，实际：$(grep -m1 '✗' "$log" || tail -2 "$log" | tr '\n' ' ')"
  fi
}

run_expect_start() {
  local desc=$1 log=$2
  shift 2
  local port
  port=$(free_port)
  "$SERVER" --package "$WORK/pkg" --keyfile "$WORK/key.txt" --answers-dir "$WORK/ans-$port" \
    --port "$port" --http-redirect-port 0 --time-check off "$@" >"$log" 2>&1 &
  local pid=$!
  local i
  for i in $(seq 1 40); do
    if grep -q '考试服务已启动' "$log"; then break; fi
    if ! kill -0 "$pid" 2>/dev/null; then break; fi
    sleep 0.5
  done
  if kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
  fi
  if grep -q '考试服务已启动' "$log"; then
    ok "$desc：正常启动"
  else
    bad "$desc：未能启动，日志：$(tail -3 "$log" | tr '\n' ' ')"
  fi
}

BASE=(--package "$WORK/pkg" --keyfile "$WORK/key.txt" --answers-dir "$WORK/ans" --port 0 --tls-host 127.0.0.1 --time-check off)

echo "外部证书自检冒烟："

# 1) 私钥与证书不配套 → 必须拒绝启动，且提示「不匹配」
run_expect_fail "私钥与证书不配套" "$WORK/1.log" "不匹配" \
  "${BASE[@]}" --tls-cert "$WORK/A/tls/server.crt" --tls-key "$WORK/B/tls/server.key"

# 2) 配套 → 必须启动，且横幅写明自检通过
# 证书 A 的 SAN 只有 127.0.0.1，所以 hosts 也只给这一个——否则会因默认 hosts 里的
# 局域网地址不在 SAN 里而产生「名称不匹配」警告（那是下一组用例要验证的事）。
run_expect_start "配套证书" "$WORK/2.log" \
  --tls-host 127.0.0.1 \
  --tls-cert "$WORK/A/tls/server.crt" --tls-key "$WORK/A/tls/server.key"
grep -q '外部证书自检通过' "$WORK/2.log" && ok "横幅含「外部证书自检通过」" ||
  bad "横幅缺少「外部证书自检通过」：$(grep -m1 '外部证书' "$WORK/2.log" || echo 无)"

# 3) SAN 不覆盖 + strict → 拒绝启动
run_expect_fail "SAN 缺地址 + strict" "$WORK/3.log" "strict" \
  "${BASE[@]}" --tls-cert "$WORK/C/tls/server.crt" --tls-key "$WORK/C/tls/server.key" --tls-check strict
# 同一套证书在默认 warn 下应当启动（只告警）
run_expect_start "SAN 缺地址 + warn" "$WORK/3b.log" \
  --tls-cert "$WORK/C/tls/server.crt" --tls-key "$WORK/C/tls/server.key"

# 4) 带方括号的 IPv6 地址要被接受（横幅会打印 https://[::1]:端口，老师很容易照抄）
run_expect_start "IPv6 方括号地址" "$WORK/4.log" \
  --tls-host '[::1]' --tls-cert "$WORK/D/tls/server.crt" --tls-key "$WORK/D/tls/server.key"

echo
if [ "$FAIL" -eq 0 ]; then
  echo "✓ 外部证书自检冒烟通过"
  exit 0
else
  echo "✗ 外部证书自检冒烟失败：$FAIL 项"
  exit 1
fi
