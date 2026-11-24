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
# 挑一个**真的能跑**的解释器：Windows 上 `python3` 可能指向应用商店占位程序，
# 一执行就失败（退出码 49），会让冒烟在莫名其妙的「python 没输出」上报错。
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
PY="$(pick_python)" || { echo "缺少可用的 Python 3.11+（脚本用了 datetime.UTC）"; exit 2; }
PORT="${EES_WEB_PORT:-$($PY -c "import socket;s=socket.socket();s.bind(('',0));print(s.getsockname()[1]);s.close()")}"
PID=""
cleanup() { [ -n "$PID" ] && kill "$PID" 2>/dev/null || true; }  # 保留 $WORK 便于失败排查
trap cleanup EXIT

[ -x "$BUILDER" ] || { echo "缺少 $BIN/exam-builder（先 cargo build --release -p builder）"; exit 1; }
[ -x "$CLIENT" ] || { echo "缺少 $BIN/exam-client"; exit 1; }
[ -x "$SERVER" ] || { echo "缺少 $BIN/exam-server"; exit 1; }

MASTER=$("$BUILDER" keygen 2>/dev/null)
START=$($PY -c "import datetime;print((datetime.datetime.now(datetime.UTC)-datetime.timedelta(minutes=30)).strftime('%Y-%m-%dT%H:%M:%SZ'))")
END=$($PY -c "import datetime;print((datetime.datetime.now(datetime.UTC)+datetime.timedelta(hours=2)).strftime('%Y-%m-%dT%H:%M:%SZ'))")
cat > "$WORK/live.json" <<JSON
{"version":2,"salt":"00112233445566778899aabbccddeeff","sessions":[
 {"session_id":1,"start":"$START","end":"$END","students":[2021001],
  "exams":[{"file":"demo/math.json","pick":3}]}]}
JSON

EXAM_MASTER=$MASTER "$BUILDER" build \
  --manifest "$WORK/live.json" --banks "$ROOT/banks" \
  --stub "$CLIENT" --out "$WORK/pkg" \
  --emit-keyfile "$WORK/key.txt" >/dev/null 2>&1

"$SERVER" --package "$WORK/pkg" --keyfile "$WORK/key.txt" \
  --port "$PORT" --answers-dir "$WORK/answers" >"$WORK/server.log" 2>&1 &
PID=$!

# 启动时先做一次网络时间校验（默认 warn 档，无外网时最多等约 6 秒），
# 所以就绪等待要给到 20 秒；上界放宽后仍用轮询，通了就立刻继续。
for _ in $(seq 1 80); do
  curl -sS -o /dev/null "http://127.0.0.1:$PORT/" && break
  sleep 0.25
done
# 这一行是 v1.4.3 新增的启动结论，缺失说明时间校验被绕过或日志被改坏。
grep -q "时间校验：" "$WORK/server.log" || { cat "$WORK/server.log"; echo "启动日志缺少时间校验结论"; exit 1; }
C="curl -sS -H Content-Type:application/json -H X-EES-CSRF:1"
# 响应先落文件再 grep：`curl | grep -q` 在 pipefail 下会假失败——grep 一匹配就退出，
# curl 写后续字节触发 EPIPE，整条管道返回非 0。响应体目前很小不会触发，但一旦变大
# 就会变成偶发红灯；仓库在 verify-embed.sh 里已经踩过并改成两段式。
$C -c "$WORK/c.txt" -d '{"student_id":2021001,"pin":null}' \
  "http://127.0.0.1:$PORT/api/login" -o "$WORK/login.json" \
  || { cat "$WORK/server.log"; echo "登录请求本身失败（curl 非零退出，服务端日志见上）"; exit 1; }
grep -q '"ok":true' "$WORK/login.json" \
  || { cat "$WORK/server.log"; echo "登录失败：$(cat "$WORK/login.json")"; exit 1; }
$C -b "$WORK/c.txt" "http://127.0.0.1:$PORT/api/paper" -o "$WORK/paper.json" \
  || { cat "$WORK/server.log"; echo "取卷请求本身失败（curl 非零退出）"; exit 1; }
QID=$($PY -c "import json,sys;d=json.load(open('$WORK/paper.json',encoding='utf-8'));print(d['paper']['sections'][0]['items'][0]['id'])")
$C -b "$WORK/c.txt" -X PUT -d "{\"qid\":\"$QID\",\"value\":\"B\"}" \
  "http://127.0.0.1:$PORT/api/answer" -o "$WORK/answer.json" \
  || { cat "$WORK/server.log"; echo "保存请求本身失败（curl 非零退出）"; exit 1; }
grep -q '"ok":true' "$WORK/answer.json" \
  || { echo "保存答案失败：$(cat "$WORK/answer.json")"; exit 1; }
$C -b "$WORK/c.txt" -X POST -d '{}' "http://127.0.0.1:$PORT/api/submit" -o "$WORK/submit.json" \
  || { cat "$WORK/server.log"; echo "交卷请求本身失败（curl 非零退出）"; exit 1; }
grep -q '"ok":true' "$WORK/submit.json" \
  || { echo "交卷失败：$(cat "$WORK/submit.json")"; exit 1; }
ls "$WORK/answers"/*.eea >/dev/null
EXAM_MASTER=$MASTER "$BUILDER" verify-answer-sheet "$WORK"/answers/*.eea \
  --package "$WORK/pkg" >"$WORK/verify.log" 2>&1
grep -q "结构 ✓" "$WORK/verify.log"
echo "✓ 网页考试冒烟通过"
