#!/usr/bin/env bash
# 答题卡链路烟测（发布前手动跑一次）
#
# 为什么需要：`EES_AUTOLOGIN` / `EES_ANSWER` 只在 debug 构建里存在，
# release 学生端没法自动作答。这个脚本用 **debug 客户端** 产生答题卡、
# 再用 **release builder** 校验，覆盖"作答 → 加密落盘 → 教师端解密"整条链路。
#
# 需要桌面环境（会真的启动 GUI 几秒），所以不放进 CI。
#
# 用法：bash scripts/smoke-answers.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CLIENT="$ROOT/target/debug/exam-client.exe"
BUILDER="$ROOT/target/release/exam-builder.exe"
WORK="$ROOT/target/smoke-answers"

# 挑一个**真的能跑**的解释器（Windows 上 python3 可能指向应用商店占位程序，
# 一执行就退出 49）；时间窗也用它生成，避免依赖 GNU `date -d`（BSD/macOS 没有）。
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

[ -x "$CLIENT" ] || { echo "✗ 缺少 debug 客户端：$CLIENT（先 cargo build -p client）"; exit 1; }
[ -x "$BUILDER" ] || { echo "✗ 缺少 release builder：$BUILDER（先 cargo build --release -p builder）"; exit 1; }

rm -rf "$WORK"
mkdir -p "$WORK/banks"

cat > "$WORK/banks/m.json" <<'JSON'
{"exam_id":"m","title":"烟测","pick":2,"questions":[
 {"id":"q1","type":"single","stem":"1+1?","options":["1","2"],"answer":"B","score":2},
 {"id":"q2","type":"multiple","stem":"偶数?","options":["2","3","4"],"answer":"AC","score":3}]}
JSON

start=$($PY -c "import datetime;print((datetime.datetime.now(datetime.UTC)-datetime.timedelta(minutes=30)).strftime('%Y-%m-%dT%H:%M:%SZ'))")
end=$($PY -c "import datetime;print((datetime.datetime.now(datetime.UTC)+datetime.timedelta(minutes=30)).strftime('%Y-%m-%dT%H:%M:%SZ'))")
cat > "$WORK/manifest.json" <<JSON
{"version":2,"salt":"00112233445566778899aabbccddeeff",
 "kdf":{"m_cost":8,"t_cost":1,"p_cost":1},
 "sessions":[{"session_id":1,"start":"$start","end":"$end","students":[2],
              "exams":[{"file":"m.json","pick":2}]}]}
JSON

MASTER="$("$BUILDER" keygen 2>/dev/null)"
EXAM_MASTER="$MASTER" "$BUILDER" build --manifest "$WORK/manifest.json" --banks "$WORK/banks" \
  --stub "$CLIENT" --out "$WORK/pkg.exe" \
  --allow-weak-kdf --force >/dev/null

# 启动 GUI 作答，轮询答题卡直到两个答案都落盘，避免固定 sleep 截断。
EES_AUTOLOGIN=2 EES_ANSWER="0=B,1=AC" "$WORK/pkg.exe" >"$WORK/gui.log" 2>&1 &
pid=$!
SHEET=""
for _ in $(seq 1 30); do
  sleep 0.5
  # 用 -print -quit 代替 `find | head -1`：head 提前关闭管道会让 find 以 141 退出，
  # 在 `set -euo pipefail` 下会把整个脚本带走。
  SHEET="$(find "$WORK" -maxdepth 1 -name 'answers-*.eea' -print -quit)"
  [ -n "$SHEET" ] || continue
  OUT="$(EXAM_MASTER="$MASTER" "$BUILDER" verify-answer-sheet "$SHEET" --package "$WORK/pkg.exe" --json 2>/dev/null || true)"
  if printf '%s' "$OUT" | "$PY" -c 'import json,sys; d=json.load(sys.stdin); assert d.get("ok") and d.get("sheet",{}).get("answers")==["B","AC"]' 2>/dev/null; then
    break
  fi
done
kill "$pid" 2>/dev/null || true
sleep 1

SHEET="$(find "$WORK" -maxdepth 1 -name 'answers-*.eea' -print -quit)"
[ -n "$SHEET" ] || { echo "✗ 没有生成答题卡"; exit 1; }
echo "答题卡：$(basename "$SHEET")（$(wc -c < "$SHEET" | tr -d ' ') 字节）"

OUT="$(EXAM_MASTER="$MASTER" "$BUILDER" verify-answer-sheet "$SHEET" --package "$WORK/pkg.exe" --json)"
echo "$OUT" | "$PY" -c '
import json,sys
d=json.load(sys.stdin)
assert d["ok"], d
assert d["binding"] is True, d
s=d["sheet"]
assert s["answers"] == ["B","AC"], s["answers"]
print("OK binding=True answers =", s["answers"])
'

echo "✓ 答题卡链路烟测通过"
