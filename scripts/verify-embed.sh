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
# 可执行文件后缀：Windows 是 .exe，Linux/macOS 为空。
# 早期版本硬编码 .exe，在 GitHub Actions（ubuntu）上直接找不到二进制。
if [ -f "$BIN/exam-builder.exe" ]; then EXE=".exe"; else EXE=""; fi

BUILDER="$BIN/exam-builder$EXE"
CLIENT="$BIN/exam-client$EXE"
[ -x "$BUILDER" ] || { echo "  ✗ 找不到 $BUILDER，请先 cargo build --release"; exit 1; }
# 注意：不要用 /tmp —— 本项目的 Windows 环境下 /tmp 写入会失败。
TMP="$ROOT/.verify-tmp"
rm -rf "$TMP"; mkdir -p "$TMP"
trap 'rm -rf "$TMP"' EXIT

fail() { echo "  ✗ $1"; exit 1; }
ok()   { echo "  ✓ $1"; }

echo "== 构建 =="
cargo build --release -p client -p builder 2>&1 | grep -E "warning: client@|Finished" || true

MASTER="$("$BUILDER" keygen 2>/dev/null)"
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
echo "== 路径 0：默认打包（包内嵌密钥，学生零配置）=="
# 这是**最重要的一条**：教师没有 Rust 工具链，必须能在不重编译 client 的
# 前提下产出学生双击可用的包。若这条失败，整个产品交付不出去。
"$BUILDER" build --manifest "$TMP/live.json" --banks banks/ \
  --stub "$CLIENT" --out "$TMP/zero_conf.exe" --master "$MASTER" 2>&1 \
  | grep -q "已内嵌密钥" || fail "默认打包未内嵌密钥"
ok "默认打包已内嵌密钥"

if "$TMP/zero_conf.exe" --student 2021001 --package "$TMP/zero_conf.exe" \
    | grep -q "解锁成功"; then
  ok "学生不带任何密钥即可解锁"
else
  fail "默认包无法零配置解锁（教师将无法交付）"
fi

"$TMP/zero_conf.exe" --student 2021001 --package "$TMP/zero_conf.exe" \
  | grep -q "正式（密钥随包分发）" || fail "模式显示不正确"
ok "模式显示为正式分发"

if "$TMP/zero_conf.exe" --student 9999999 --package "$TMP/zero_conf.exe" >/dev/null 2>&1; then
  fail "默认包中来宾学号竟然解锁成功"
fi
ok "名单外学生被拒绝"

echo
echo "== 路径 1：显式 --no-embed-key（调试模式）=="
"$BUILDER" build --manifest "$TMP/live.json" --banks banks/ \
  --stub "$CLIENT" --out "$TMP/dev.exe" --master "$MASTER" --no-embed-key >/dev/null 2>&1

if "$TMP/dev.exe" --student 2021001 --package "$TMP/dev.exe" >/dev/null 2>&1; then
  fail "未嵌入时竟然不需要 master"
fi
ok "不带 master 时正确报错"

"$TMP/dev.exe" --student 2021001 --master "$MASTER" --package "$TMP/dev.exe" \
  | grep -q "解锁成功" || fail "带 master 时未能解锁"
ok "带 master 可解锁"

echo
echo "== 路径 2：已嵌入（发布模式）=="
"$BUILDER" build --manifest "$TMP/live.json" --banks banks/ \
  --stub "$CLIENT" --out "$TMP/ignored.exe" --master "$MASTER" \
  --emit-keyfile "$TMP/kf.txt" >/dev/null 2>&1
[ "$(wc -c < "$TMP/kf.txt")" -eq 64 ] || fail "keyfile 长度不是 64"

EXAM_KEYFILE="$TMP/kf.txt" cargo build --release -p client 2>&1 \
  | grep -q "已嵌入 K_manifest" || fail "build.rs 未识别 EXAM_KEYFILE"
ok "build.rs 已嵌入密钥"

cp "$CLIENT" "$TMP/stub_emb.exe"
"$BUILDER" build --manifest "$TMP/live.json" --banks banks/ \
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
echo "== stub 身份检测（防止拿错二进制当学生端）=="
# v3 起密钥默认写在包里，stub 是否编译期内嵌已不影响学生能否使用；
# 这里改为验证：非本项目的 exe 必须被识别出来。
cargo build --release -p client >/dev/null 2>&1
OUT=$("$BUILDER" build --manifest "$TMP/live.json" --banks banks/ \
      --stub "$CLIENT" --out "$TMP/plain.exe" --master "$MASTER" 2>&1)
echo "$OUT" | grep -q "没有密钥状态标记" && fail "本项目的 client 被误判为陌生 stub"
ok "本项目 client 被正确识别"

# 非本项目的 exe 必须提示无标记
printf 'NOT_A_CLIENT' > "$TMP/fake.exe"
OUT=$("$BUILDER" build --manifest "$TMP/live.json" --banks banks/ \
      --stub "$TMP/fake.exe" --out "$TMP/fake_out.exe" --master "$MASTER" 2>&1)
echo "$OUT" | grep -q "没有密钥状态标记" || fail "非本项目 exe 未被识别"
ok "非本项目 exe 被正确识别"

# 二进制里两个标记不能同时存在
grep -aq "EXAMKEY:EMBEDDED" "$TMP/stub_emb.exe" || fail "嵌入版缺少 EMBEDDED 标记"
if grep -aq "EXAMKEY:PLAIN" "$TMP/stub_emb.exe"; then
  fail "嵌入版同时含有 PLAIN 标记（状态判定会出错）"
fi
ok "标记互斥"

echo
echo "✓ 密钥嵌入验收全部通过"
