#!/usr/bin/env bash
# 网页考试端到端冒烟：打包 → 启动 exam-server → 登录/取卷/作答/交卷 → 校验 .eea
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
BIN="$ROOT/target/release"
EXE=""
[ -f "$BIN/exam-server.exe" ] && EXE=".exe"
BUILDER="$BIN/exam-builder$EXE"; CLIENT="$BIN/exam-client$EXE"; SERVER="$BIN/exam-server$EXE"
WORK="target/web-smoke"
rm -rf "$WORK"
mkdir -p "$WORK"
PORT="${EES_WEB_PORT:-$(python -c "import socket;s=socket.socket();s.bind(('',0));print(s.getsockname()[1]);s.close()")}"
PID=""
cleanup() { [ -n "$PID" ] && kill "$PID" 2>/dev/null || true; }  # 保留 $WORK 便于失败排查
trap cleanup EXIT

[ -x "$BUILDER" ] || { echo "缺少 $BIN/exam-builder（先 cargo build --release -p builder）"; exit 1; }
[ -x "$CLIENT" ] || { echo "缺少 $BIN/exam-client"; exit 1; }
[ -x "$SERVER" ] || { echo "缺少 $BIN/exam-server"; exit 1; }

MASTER=$("$BUILDER" keygen 2>/dev/null)
START=$(date -u -d '30 minutes ago' +%Y-%m-%dT%H:%M:%SZ)
END=$(date -u -d '2 hours' +%Y-%m-%dT%H:%M:%SZ)
cat > "$WORK/live.json" <<JSON
{"version":2,"salt":"00112233445566778899aabbccddeeff","sessions":[
 {"session_id":1,"start":"$START","end":"$END","students":[2021001],
  "exams":[{"file":"demo/math.json","pick":3}]}]}
JSON

EXAM_MASTER=$MASTER "$BUILDER" build \
  --manifest "$WORK/live.json" --banks "$ROOT/banks" \
  --stub "$CLIENT" --out "$WORK/pkg" \
  --emit-keyfile "$WORK/key.txt" --master "$MASTER" >/dev/null 2>&1

"$SERVER" --package "$WORK/pkg" --keyfile "$WORK/key.txt" \
  --port "$PORT" --answers-dir "$WORK/answers" >"$WORK/server.log" 2>&1 &
PID=$!

for _ in $(seq 1 40); do
  curl -sS -o /dev/null "http://127.0.0.1:$PORT/" && break
  sleep 0.25
done
C="curl -sS -H Content-Type:application/json -H X-EES-CSRF:1"
$C -c "$WORK/c.txt" -d '{"student_id":2021001,"pin":null}' "http://127.0.0.1:$PORT/api/login" | grep -q '"ok":true' || { cat "$WORK/server.log"; exit 1; }
$C -b "$WORK/c.txt" "http://127.0.0.1:$PORT/api/paper" -o "$WORK/paper.json"
QID=$(python -c "import json,sys;d=json.load(open('$WORK/paper.json',encoding='utf-8'));print(d['paper']['sections'][0]['items'][0]['id'])")
$C -b "$WORK/c.txt" -X PUT -d "{\"qid\":\"$QID\",\"value\":\"B\"}" "http://127.0.0.1:$PORT/api/answer" | grep -q '"ok":true'
$C -b "$WORK/c.txt" -X POST -d '{}' "http://127.0.0.1:$PORT/api/submit" | grep -q '"ok":true'
ls "$WORK/answers"/*.eea >/dev/null
"$BUILDER" verify-answer-sheet "$WORK"/answers/*.eea --package "$WORK/pkg" --master "$MASTER" >"$WORK/verify.log" 2>&1
grep -q "结构 ✓" "$WORK/verify.log"
echo "✓ 网页考试冒烟通过"
