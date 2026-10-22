#!/usr/bin/env bash
# 密钥嵌入端到端验收。
#
# 验证两条路径：
#   1. 未嵌入（开发模式）：必须显式提供 master
#   2. 已嵌入（发布模式）：不带任何密钥即可解锁
#
# 用法：在仓库根目录执行  bash scripts/verify-embed.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BIN="$ROOT/target/release"
# 注意：不要用 /tmp —— 本项目的 Windows 环境下 /tmp 写入会失败。
TMP="$ROOT/.verify-tmp"
rm -rf "$TMP"; mkdir -p "$TMP"
trap 'rm -rf "$TMP"' EXIT

fail() { echo "  ✗ $1"; exit 1; }
ok()   { echo "  ✓ $1"; }

echo "== 构建 =="
cargo build --release -p client -p builder 2>&1 | grep -E "warning: client@|Finished" || true

MASTER="$("$BIN/exam-builder.exe" keygen 2>/dev/null)"
[ "${#MASTER}" -eq 64 ] || fail "keygen 输出长度不是 64"

# 动态时间窗：写死日期会在日期过后永久失败
NS=$(date -u -d '1 hour ago' +%Y-%m-%dT%H:%M:%SZ)
NE=$(date -u -d '1 hour'     +%Y-%m-%dT%H:%M:%SZ)
cat > "$TMP/live.json" <<JSON
{
  "version": 2,
  "salt": "00112233445566778899aabbccddeeff",
  "sessions": [{
    "session_id": 1,
    "start": "$NS", "end": "$NE",
    "students": [2021001, 2021002],
    "exams": [{ "file": "demo/math.json", "pick": 2 }]
  }]
}
JSON

echo
echo "== 路径 1：未嵌入（开发模式）=="
"$BIN/exam-builder.exe" build --manifest "$TMP/live.json" --banks banks/ \
  --stub "$BIN/exam-client.exe" --out "$TMP/dev.exe" --master "$MASTER" >/dev/null 2>&1

if "$TMP/dev.exe" --student 2021001 --package "$TMP/dev.exe" >/dev/null 2>&1; then
  fail "未嵌入时竟然不需要 master"
fi
ok "不带 master 时正确报错"

"$TMP/dev.exe" --student 2021001 --master "$MASTER" --package "$TMP/dev.exe" \
  | grep -q "解锁成功" || fail "带 master 时未能解锁"
ok "带 master 可解锁"

echo
echo "== 路径 2：已嵌入（发布模式）=="
"$BIN/exam-builder.exe" build --manifest "$TMP/live.json" --banks banks/ \
  --stub "$BIN/exam-client.exe" --out "$TMP/ignored.exe" --master "$MASTER" \
  --emit-keyfile "$TMP/kf.txt" >/dev/null 2>&1
[ "$(wc -c < "$TMP/kf.txt")" -eq 64 ] || fail "keyfile 长度不是 64"

EXAM_KEYFILE="$TMP/kf.txt" cargo build --release -p client 2>&1 \
  | grep -q "已嵌入 K_manifest" || fail "build.rs 未识别 EXAM_KEYFILE"
ok "build.rs 已嵌入密钥"

cp "$BIN/exam-client.exe" "$TMP/stub_emb.exe"
"$BIN/exam-builder.exe" build --manifest "$TMP/live.json" --banks banks/ \
  --stub "$TMP/stub_emb.exe" --out "$TMP/student.exe" --master "$MASTER" >/dev/null 2>&1

"$TMP/student.exe" --student 2021001 --package "$TMP/student.exe" \
  | grep -q "解锁成功" || fail "嵌入后仍无法免密解锁"
ok "学生无需任何密钥即可解锁"

if "$TMP/student.exe" --student 9999999 --package "$TMP/student.exe" >/dev/null 2>&1; then
  fail "名单外学生竟然解锁成功"
fi
ok "名单外学生被拒绝"

echo
echo "== 安全验收 =="
if grep -aq "$MASTER" "$TMP/student.exe"; then fail "exe 中出现 master 明文"; fi
ok "搜不到 master 明文"

if grep -aq "$(cat "$TMP/kf.txt")" "$TMP/student.exe"; then
  fail "exe 中出现 K_manifest 明文（分片混淆失效）"
fi
ok "搜不到 K_manifest 明文"

if grep -aq "2021001" "$TMP/student.exe"; then fail "exe 中出现学号明文"; fi
ok "搜不到学号明文"

echo
echo "== 一致性校验 =="
# 嵌入版 + 显式 master（不一致时应报错，一致时应通过）
"$TMP/student.exe" --student 2021001 --master "$MASTER" --package "$TMP/student.exe" \
  | grep -q "解锁成功" || fail "嵌入版传入一致的 master 时应当通过"
ok "显式 master 与嵌入值一致时通过"

WRONG="$(printf 'ab%.0s' $(seq 1 32))"
if "$TMP/student.exe" --student 2021001 --master "$WRONG" --package "$TMP/student.exe" \
     >/dev/null 2>&1; then
  fail "不一致的 master 未被拒绝"
fi
ok "不一致的 master 被拒绝"

# 还原未嵌入状态，避免污染后续构建
cargo build --release -p client >/dev/null 2>&1

echo
echo "✓ 密钥嵌入验收全部通过"
