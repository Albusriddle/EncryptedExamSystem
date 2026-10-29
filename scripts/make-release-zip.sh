#!/usr/bin/env bash
# 生成面向教师的发布包：解压后即可完成全部操作。
#
# 产物结构（teacher 视角，一眼能看懂要做什么）：
#   ExamAdmin-vX.Y.Z/
#   ├── 开始这里.md              ← 第一步该看的东西
#   ├── 使用说明.md              ← 完整手册
#   ├── ExamAdmin/               ← 管理端程序
#   │   ├── ExamAdmin.exe
#   │   ├── stub-template.exe
#   │   └── _internal/
#   └── 示例题库/                ← 拿来就能试的题库
#       ├── math.json
#       └── english.json
#
# 用法：bash scripts/make-release-zip.sh [版本号]

set -euo pipefail

# 脚本里内嵌的 Python 会打印中文。Windows runner 的 Python 默认用
# cp1252/cp936 编码 stdout，遇到中文直接 UnicodeEncodeError。
# 统一强制 UTF-8，避免"本地能跑、CI 报编码错"。
export PYTHONIOENCODING=utf-8

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VER="${1:-$(git describe --tags --always 2>/dev/null || echo dev)}"
OUTDIR="$ROOT/dist"
STAGE="$OUTDIR/ExamAdmin-$VER"

echo "== 准备发布包 ExamAdmin-$VER =="

if [ ! -f "$OUTDIR/ExamAdmin/ExamAdmin.exe" ] && [ ! -f "$OUTDIR/ExamAdmin/ExamAdmin" ]; then
  echo "  管理端尚未打包，先执行："
  echo "    python admin/buildtools/build_exe.py"
  exit 1
fi

rm -rf "$STAGE"
mkdir -p "$STAGE"

# 1. 管理端程序（整个文件夹，含 _internal）
cp -r "$OUTDIR/ExamAdmin" "$STAGE/ExamAdmin"
# 去掉模板与手册的副本，改放到外层，避免重复
rm -f "$STAGE/ExamAdmin/stub-template.exe" "$STAGE/ExamAdmin/使用说明.md"
echo "  ✓ ExamAdmin/"

# 2. 学生端模板（打包时必须用，教师不需理解它）
cp "$OUTDIR/ExamAdmin/_internal/exam-builder.exe" "$STAGE/ExamAdmin/" 2>/dev/null || true
cp "$ROOT/target/release/exam-client.exe" "$STAGE/ExamAdmin/stub-template.exe" 2>/dev/null \
  || cp "$ROOT/target/x86_64-pc-windows-msvc/release/exam-client.exe" "$STAGE/ExamAdmin/stub-template.exe"
echo "  ✓ ExamAdmin/ 内含 stub-template.exe 与 exam-builder.exe"

# 3. 示例题库（拿来就能试）
#
# 不能直接 cp banks/demo/*.json：那些文件的 exam_id 是 demo-math / demo-english，
# 而文件名是 math.json / english.json，两者对不上。教师在 GUI 里导入后，
# 场次引用会写成 exam_id.json，一旦有人手工改成文件名就会匹配失败。
# 这里生成 exam_id 与文件名一致的示例，消除这个坑。
mkdir -p "$STAGE/示例题库"
python - "$ROOT" "$STAGE/示例题库" <<'PYGEN'
import json
import sys
from pathlib import Path

root = Path(sys.argv[1])
out = Path(sys.argv[2])
for src_name, new_id, title, out_name in [
    ("math.json", "math", "数学（示例）", "数学.json"),
    ("english.json", "english", "英语（示例）", "英语.json"),
]:
    data = json.loads((root / "banks" / "demo" / src_name).read_text(encoding="utf-8"))
    data["exam_id"] = new_id
    data["title"] = title
    (out / out_name).write_text(
        json.dumps(data, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    print(f"     {out_name}: exam_id={new_id}, {len(data['questions'])} 题")
PYGEN
# 图片题示例：放到根 示例题库/，与手册里的路径一致
if [ -f "$ROOT/examples/image-question.json" ]; then
  cp "$ROOT/examples/image-question.json" "$STAGE/示例题库/图片题示例.json"
  echo "  ✓ 示例题库/图片题示例.json"
fi

echo "  ✓ 示例题库/（exam_id 与文件名一致）"

# 4. 手册
cp "$ROOT/docs/教师使用手册.md" "$STAGE/使用说明.md"
cp "$ROOT/docs/风险接受记录.md" "$STAGE/风险接受记录.md"
echo "  ✓ 使用说明.md + 风险接受记录.md"

# 5. 入口页：告诉老师从哪开始
cat > "$STAGE/开始这里.md" <<'DOC'
# 从这里开始

## 一、打开程序

1. 进入 `ExamAdmin` 文件夹
2. 双击 **`ExamAdmin.exe`**

> ⚠ 不要把 `ExamAdmin.exe` 单独拖到别处，它需要同目录的 `_internal` 才能启动。

## 二、完整流程（约 10 分钟）

| 步骤 | 在哪 | 做什么 |
|---|---|---|
| 1 | `[题库]` 标签 | 点「导入题库…」，选 `示例题库\数学.json` |
| 2 | `[学生]` 标签 | 粘贴学号（每行一个），点「应用为学号名单」 |
| 3 | `[场次]` 标签 | 点「新建」，设考试时间，勾选科目，点「保存此场次」；需要控制难度时勾选「限制平均难度」（1–255） |
| 4 | `[打包]` 标签 | 点「生成」拿主密钥（有进度条，约 1–2 秒）→ 点「开始打包」 |
| 5 | 分发 | 把生成的 `.exe` 发给学生，双击输入学号即可 |
| 6 | `[收卷判分]` | 考后拖入学生交的 `.eea` → 校验 → 生成成绩 → 导出已判/未判 CSV |

> 启动是空白工程；要接着上次做，点工具栏【打开上次工程】。
> 勾选「每生 PIN 登录」时，PIN 表在打包成功弹窗里显示路径（工程保存位置，不在考试包目录）。

**先把 `示例题库` 里的三个文件跑一遍**，确认流程通了，再换成你自己的题库。

## 三、题库格式

`示例题库\数学.json` 就是模板，照它的结构写即可。
关键字段：`exam_id`（唯一标识）、`title`（学生看到的科目名）、
`pick`（抽几题）、`questions[]`（题目）。

**文件必须是 UTF-8 编码**，否则程序会提示。

## 四、必须记住的一件事

第 4 步生成的**主密钥**是全系统最重要的东西：

- 它不在任何生成的包里，也不在工程文件里
- 丢了不影响已经发出去的包（密钥已写进包内）
- 但**无法再打新包**给学生
- 请存进密码管理器，或抄在纸上锁起来

## 五、更多

详细说明、常见报错对照、安全须知 → 见 `使用说明.md`。
DOC
echo "  ✓ 开始这里.md"

# 6. 压缩
cd "$OUTDIR"
ZIP="ExamAdmin-$VER-windows.zip"
rm -f "$ZIP"
if command -v powershell.exe >/dev/null 2>&1; then
  powershell.exe -NoProfile -Command \
    "Compress-Archive -Path '$(pwd -W)/ExamAdmin-$VER' -DestinationPath '$(pwd -W)/$ZIP' -Force" >/dev/null
else
  zip -qr "$ZIP" "ExamAdmin-$VER"
fi
echo "  ✓ $ZIP（$(python -c "import os,sys;print(f\"{os.path.getsize(sys.argv[1])/1048576:.0f} MB\")" "$ZIP" 2>/dev/null || echo "大小未知")）"

echo
echo "== 发布包内容 =="
find "ExamAdmin-$VER" -maxdepth 1 -mindepth 1 | sed 's|.*/|  |'
echo
echo "资产路径：$OUTDIR/$ZIP"
