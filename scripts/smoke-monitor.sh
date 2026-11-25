#!/usr/bin/env bash
# 监考页冒烟：路由 / 令牌鉴权 / 收卷 / 明文写操作策略 / 弱令牌拒绝。
#
# 为什么需要它：v1.8.0 的监考页只有 Rust 单测与一次性前端桩测试，`scripts/` 与 CI 里
# 没有一个字提到 monitor——路由写错、`include_str!` 路径打错、响应头丢了 `no-store`，
# 门禁都会照样全绿。这里用纯 curl 把「页面能打开、令牌能鉴别、收卷能出卡」钉住。
set -uo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN=${BIN:-}
if [ -z "$BIN" ]; then
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

WORK="$ROOT/target/monitor-smoke"
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
 {"session_id":1,"start":"$START","end":"$END","students":[2021001,2021002],
  "exams":[{"file":"demo/math.json","pick":2}]}]}
JSON
printf '2021001\n2021002\n' >"$WORK/roster.txt"
EXAM_MASTER=$MASTER "$BUILDER" build --manifest "$WORK/live.json" --banks "$ROOT/banks" \
  --stub "$CLIENT" --out "$WORK/pkg" --emit-keyfile "$WORK/key.txt" >"$WORK/build.log" 2>&1 ||
  { echo "打包失败："; tail -3 "$WORK/build.log"; exit 1; }

# ---------------------------------------------------------------- 起服务
PORT=$(free_port)
ANS="$WORK/ans"
"$SERVER" --package "$WORK/pkg" --keyfile "$WORK/key.txt" --answers-dir "$ANS" \
  --port "$PORT" --http-redirect-port 0 --time-check off --login-rate-limit 0 \
  --tls off --roster "$WORK/roster.txt" >"$WORK/server.log" 2>&1 &
PID=$!
cleanup() {
  kill "$PID" 2>/dev/null
  wait "$PID" 2>/dev/null
}
trap cleanup EXIT

for _ in $(seq 1 60); do
  curl -sS -o /dev/null "http://127.0.0.1:$PORT/monitor" && break
  sleep 0.5
done
grep -q '考试服务已启动' "$WORK/server.log" ||
  { echo "服务未就绪："; tail -5 "$WORK/server.log"; exit 1; }

TOKEN=$(tr -d '\r\n' <"$ANS/monitor.token")
BASE="http://127.0.0.1:$PORT"

echo "监考页冒烟："

# 1) 页面能打开，且不缓存（令牌在 URL 里，缓存一份等于泄漏）
HDR=$(curl -sS -D - -o /dev/null "$BASE/monitor")
echo "$HDR" | grep -qi '^HTTP/1.1 200' && ok "GET /monitor → 200" || bad "GET /monitor 状态异常：$(echo "$HDR" | head -1)"
echo "$HDR" | grep -qi 'Cache-Control: no-store' && ok "页面 no-store" || bad "页面缺 Cache-Control: no-store"

# 2) 没有令牌不能读
CODE=$(curl -sS -o /dev/null -w '%{http_code}' "$BASE/api/monitor/stats")
[ "$CODE" = "401" ] && ok "无令牌 stats → 401" || bad "无令牌 stats 返回 $CODE（应为 401）"

# 3) 有令牌能读，字段齐全
curl -sS -H "X-Monitor-Token: $TOKEN" "$BASE/api/monitor/stats" -o "$WORK/stats.json"
"$PY" - "$WORK/stats.json" <<'PY' && ok "stats 字段齐全（counts/students/roster_known）" || bad "stats 字段缺失（见上）"
import json, sys
d = json.load(open(sys.argv[1], encoding="utf-8"))
assert isinstance(d.get("counts"), dict), d
assert isinstance(d.get("students"), list), d
assert "roster_known" in d, d
assert d["counts"].get("roster") == 2, d["counts"]
PY

# 4) 学生登录 + 监考收卷 → 出现 .eea
curl -sS -c "$WORK/c.txt" -H 'Content-Type: application/json' -H 'X-EES-CSRF: 1' \
  -d '{"student_id":2021001}' "$BASE/api/login" -o "$WORK/login.json"
# 题号不能写死：抽题是按 (学号,场次,salt) 确定的，硬编码 qid 换个题库就 400。
curl -sS -b "$WORK/c.txt" "$BASE/api/paper" -o "$WORK/paper.json"
QID=$("$PY" - "$WORK/paper.json" <<'PYQ'
import json, sys
d = json.load(open(sys.argv[1], encoding="utf-8"))
print(d["paper"]["sections"][0]["items"][0]["id"])
PYQ
)
CODE=$(curl -sS -b "$WORK/c.txt" -H 'Content-Type: application/json' -H 'X-EES-CSRF: 1' \
  -d "{\"qid\":\"$QID\",\"value\":\"hello\"}" -X PUT "$BASE/api/answer" -o /dev/null -w '%{http_code}')
[ "$CODE" = "200" ] && ok "学生保存作答 → 200" || bad "学生保存返回 $CODE"

CODE=$(curl -sS -H "X-Monitor-Token: $TOKEN" -H 'Content-Type: application/json' -H 'X-EES-CSRF: 1' \
  -d '{"student_id":2021001}' -X POST "$BASE/api/monitor/collect" -o "$WORK/collect.json" -w '%{http_code}')
[ "$CODE" = "200" ] && ok "监考收卷 → 200" || bad "监考收卷返回 $CODE：$(cat "$WORK/collect.json")"
ls "$ANS"/*.eea >/dev/null 2>&1 && ok "答题卡 .eea 已落盘" || bad "没有生成 .eea"

# 5) 收卷后学生再写必须被拒（封存语义）
CODE=$(curl -sS -b "$WORK/c.txt" -H 'Content-Type: application/json' -H 'X-EES-CSRF: 1' \
  -d "{\"qid\":\"$QID\",\"value\":\"late\"}" -X PUT "$BASE/api/answer" -o /dev/null -w '%{http_code}')
[ "$CODE" = "401" ] || [ "$CODE" = "409" ] && ok "收卷后学生再写被拒（$CODE）" || bad "收卷后学生仍能写：$CODE"
cleanup
trap - EXIT

# 6) tls-only + 明文 → 写操作 403
PORT2=$(free_port)
ANS2="$WORK/ans2"
"$SERVER" --package "$WORK/pkg" --keyfile "$WORK/key.txt" --answers-dir "$ANS2" \
  --port "$PORT2" --http-redirect-port 0 --time-check off --login-rate-limit 0 \
  --tls off --roster "$WORK/roster.txt" --monitor-write-mode tls-only >"$WORK/server2.log" 2>&1 &
PID2=$!
for _ in $(seq 1 60); do
  curl -sS -o /dev/null "http://127.0.0.1:$PORT2/monitor" && break
  sleep 0.5
done
TOKEN2=$(tr -d '\r\n' <"$ANS2/monitor.token")
CODE=$(curl -sS -H "X-Monitor-Token: $TOKEN2" -H 'Content-Type: application/json' -H 'X-EES-CSRF: 1' \
  -d '{"student_id":2021001}' -X POST "http://127.0.0.1:$PORT2/api/monitor/collect" -o /dev/null -w '%{http_code}')
[ "$CODE" = "403" ] && ok "tls-only 下明文写 → 403" || bad "tls-only 下明文写返回 $CODE（应为 403）"
kill "$PID2" 2>/dev/null
wait "$PID2" 2>/dev/null

# 7) 弱令牌必须拒绝启动（否则「替全班收卷」的凭据可被枚举）
"$SERVER" --package "$WORK/pkg" --keyfile "$WORK/key.txt" --answers-dir "$WORK/ans3" \
  --port "$(free_port)" --http-redirect-port 0 --time-check off \
  --tls off --monitor-token abc >"$WORK/weak.log" 2>&1
if [ $? -ne 0 ] && grep -q 'monitor-token' "$WORK/weak.log"; then
  ok "弱令牌 --monitor-token abc → 拒绝启动"
else
  bad "弱令牌被接受了（见 $WORK/weak.log）"
fi

echo
if [ "$FAIL" -eq 0 ]; then
  echo "✓ 监考页冒烟通过"
  exit 0
else
  echo "✗ 监考页冒烟失败：$FAIL 项"
  exit 1
fi
