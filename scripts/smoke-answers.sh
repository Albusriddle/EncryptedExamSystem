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

[ -x "$CLIENT" ] || { echo "✗ 缺少 debug 客户端：$CLIENT（先 cargo build -p client）"; exit 1; }
[ -x "$BUILDER" ] || { echo "✗ 缺少 release builder：$BUILDER（先 cargo build --release -p builder）"; exit 1; }

rm -rf "$WORK"
mkdir -p "$WORK/banks"

cat > "$WORK/banks/m.json" <<'JSON'
{"exam_id":"m","title":"烟测","pick":2,"questions":[
 {"id":"q1","type":"single","stem":"1+1?","options":["1","2"],"answer":"B","score":2},
 {"id":"q2","type":"multiple","stem":"偶数?","options":["2","3","4"],"answer":"AC","score":3}]}
JSON

start=$(date -u -d '30 minutes ago' +%Y-%m-%dT%H:%M:%SZ)
end=$(date -u -d '30 minutes' +%Y-%m-%dT%H:%M:%SZ)
cat > "$WORK/manifest.json" <<JSON
{"version":2,"salt":"00112233445566778899aabbccddeeff",
 "kdf":{"m_cost":8,"t_cost":1,"p_cost":1},
 "sessions":[{"session_id":1,"start":"$start","end":"$end","students":[2],
              "exams":[{"file":"m.json","pick":2}]}]}
JSON

MASTER="$("$BUILDER" keygen 2>/dev/null)"
"$BUILDER" build --manifest "$WORK/manifest.json" --banks "$WORK/banks" \
  --stub "$CLIENT" --out "$WORK/pkg.exe" --master "$MASTER" \
  --allow-weak-kdf --force >/dev/null

# 启动 GUI 作答 6 秒后杀掉
EES_AUTOLOGIN=2 EES_ANSWER="0=B,1=AC" "$WORK/pkg.exe" >"$WORK/gui.log" 2>&1 &
pid=$!
sleep 6
kill "$pid" 2>/dev/null || true
sleep 1

SHEET="$(find "$WORK" -maxdepth 1 -name 'answers-*.eea' | head -1)"
[ -n "$SHEET" ] || { echo "✗ 没有生成答题卡"; exit 1; }
echo "答题卡：$(basename "$SHEET")（$(wc -c < "$SHEET" | tr -d ' ') 字节）"

OUT="$(EXAM_MASTER="$MASTER" "$BUILDER" verify-answer-sheet "$SHEET" --package "$WORK/pkg.exe" --json)"
echo "$OUT" | python -c '
import json,sys
d=json.load(sys.stdin)
assert d["ok"], d
assert d["binding"] is True, d
s=d["sheet"]
assert s["answers"] == ["B","AC"], s["answers"]
print("OK binding=True answers =", s["answers"])
'

echo "✓ 答题卡链路烟测通过"
