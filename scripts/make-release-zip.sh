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
# 版本号会拼进路径，而下面有 rm -rf。不放白名单的话，手动传 `../x`/`--full`
# 这类参数会把 rm 指到非预期位置、或造出 `ExamAdmin---full` 这种怪包。
# 允许多一段 `-N-g<sha>`（git describe 的形状），但不允许以 `-` 开头、不含空串。
case "$VER" in
  "" | -* | *[!A-Za-z0-9._-]*)
    echo "✗ 版本号不合法：$VER（只允许字母、数字、点、下划线、连字符，且不能以 - 开头）"
    exit 1
    ;;
esac
OUTDIR="$ROOT/dist"
STAGE="$OUTDIR/ExamAdmin-$VER"

echo "== 准备发布包 ExamAdmin-$VER =="

if [ ! -f "$OUTDIR/ExamAdmin/ExamAdmin.exe" ] && [ ! -f "$OUTDIR/ExamAdmin/ExamAdmin" ]; then
  echo "  管理端尚未打包，先执行："
  echo "    python admin/buildtools/build_exe.py"
  exit 1
fi

if [ -f "$OUTDIR/ExamAdmin/ExamAdmin.exe" ]; then
  EXE=".exe"
  PLATFORM="windows"
elif [ "$(uname -s)" = Linux ]; then
  EXE=""
  PLATFORM="linux"
else
  echo "  不支持此平台的发布包。"
  exit 1
fi

rm -rf "$STAGE"
mkdir -p "$STAGE"

# 1. 管理端程序（整个文件夹，含 _internal）
cp -r "$OUTDIR/ExamAdmin" "$STAGE/ExamAdmin"
# 手册改放到发布包根；学生端模板保留在程序旁边。
rm -f "$STAGE/ExamAdmin/使用说明.md"
echo "  ✓ ExamAdmin/"

# 2. 学生端模板（打包时必须用，教师不需理解它）
# 缺少打包器或模板必须失败，不能把坏包说成好包；Linux 同时保留可执行权限。
for name in "exam-builder$EXE" "exam-server$EXE"; do
  if [ -f "$OUTDIR/ExamAdmin/_internal/$name" ]; then
    cp -p "$OUTDIR/ExamAdmin/_internal/$name" "$STAGE/ExamAdmin/"
  fi
done
# 优先保留 build_exe.py 随包复制的模板，避免打包后又换成旧 target 产物。
if [ ! -f "$STAGE/ExamAdmin/stub-template$EXE" ]; then
  for candidate in "$ROOT/target/release/exam-client$EXE" "$ROOT"/target/*/release/"exam-client$EXE"; do
    if [ -f "$candidate" ]; then
      cp -p "$candidate" "$STAGE/ExamAdmin/stub-template$EXE"
      break
    fi
  done
fi
for name in "stub-template$EXE" "exam-builder$EXE" "exam-server$EXE"; do
  [ -f "$STAGE/ExamAdmin/$name" ] || { echo "  ✗ 缺少 $name"; exit 1; }
done
echo "  ✓ ExamAdmin/ 内含原生学生端模板、打包器与网页服务端"

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

# 4. 文档：全部随包发布，保证手册里的相对链接都能打开
mkdir -p "$STAGE/docs"
cp "$ROOT"/docs/*.md "$STAGE/docs/"
cp "$ROOT/docs/教师使用手册.md" "$STAGE/使用说明.md"
cp "$ROOT/docs/风险接受记录.md" "$STAGE/风险接受记录.md"
echo "  ✓ docs/（$(ls "$STAGE/docs" | wc -l | tr -d ' ') 份）+ 使用说明.md"

# 4.1 题库自检工具（手册 §2.4 要用，必须随包发布）
mkdir -p "$STAGE/tools"
cp -r "$ROOT/tools/bank_qc" "$STAGE/tools/bank_qc"
rm -rf "$STAGE/tools/bank_qc/__pycache__"
echo "  ✓ tools/bank_qc/"

# 4.2 示例题库的定位说明（它不满足出题口径，门禁会 FAIL）
cat > "$STAGE/示例题库/README.md" <<'DOC'
# 示例题库（仅演示流程）

这三个文件用来**跑通管理端流程**，不是出题模板，也**不满足**《出题质检与难度口径》：

- 题量、难度分布、K/A/C 标注都是为了演示而随意填的；
- 用 `tools/bank_qc/verify_bank.py` 检查本目录会得到 `FAIL`，这是**预期结果**；
- 真正出题时请按 `docs/题库JSON规范.md` 与 `docs/出题质检与难度口径.md` 编写。

先导入本目录的文件、走完「导入 → 场次 → 打包 → 收卷」一遍，再换成自己的题库。
DOC
echo "  ✓ 示例题库/README.md"

# 4.3 把手册里的链接改写成发布包内的真实路径
python - "$STAGE" <<'PYFIX'
import re
import sys
from pathlib import Path

stage = Path(sys.argv[1])
doc_names = sorted(p.name for p in (stage / "docs").glob("*.md"))
manual = stage / "使用说明.md"
text = manual.read_text(encoding="utf-8")
# 手册在仓库里与 docs 同级；发布包里被放到根，链接要补 docs/ 前缀。
text = text.replace("](../tools/bank_qc/README.md)", "](tools/bank_qc/README.md)")
for name in doc_names:
    text = text.replace(f"]({name})", f"](docs/{name})")
manual.write_text(text, encoding="utf-8", newline="\n")

# 风险接受记录在仓库里与 docs 同级；发布包根也有一份（兼容旧链接），同样补前缀。
risk = stage / "风险接受记录.md"
if risk.is_file():
    t = risk.read_text(encoding="utf-8")
    for name in doc_names:
        t = t.replace(f"]({name})", f"](docs/{name})")
    risk.write_text(t, encoding="utf-8", newline="\n")

# build_exe.py 会把图片教程复制到 ExamAdmin/，那里的链接同样要指向 docs/。
tutorial = stage / "ExamAdmin" / "图片使用教程.md"
if tutorial.is_file():
    t = tutorial.read_text(encoding="utf-8")
    for name in doc_names:
        t = t.replace(f"]({name})", f"](../docs/{name})")
    tutorial.write_text(t, encoding="utf-8", newline="\n")
print("    链接已改写")
PYFIX

# 4.4 死链自检：发布包里任何 .md 的相对链接都必须能打开
python - "$STAGE" <<'PYLINK'
import re
import sys
from pathlib import Path

stage = Path(sys.argv[1]).resolve()
bad = []
for path in stage.rglob("*.md"):
    text = path.read_text(encoding="utf-8")
    # 用按行状态机剥掉围栏代码块：非贪婪正则在"围栏未闭合"时会一路剥到文末，
    # 让后面的真链接静默跳过检查。围栏数量必须成对，否则直接失败。
    lines = text.splitlines(keepends=True)
    fences = sum(1 for line in lines if line.lstrip().startswith("```"))
    if fences % 2:
        print(f"  ✗ {path.relative_to(stage)} 的代码围栏没有成对（共 {fences} 个）")
        sys.exit(1)
    kept = []
    in_code = False
    for line in lines:
        if line.lstrip().startswith("```"):
            in_code = not in_code
            continue
        if not in_code:
            kept.append(line)
    text = "".join(kept)
    # 行内代码同样不是链接。
    text = re.sub(r"`[^`]*`", "", text)
    for m in re.finditer(r"\[[^\]]*\]\(([^)#]+)(?:#[^)]*)?\)", text):
        target = m.group(1).strip()
        if target.startswith(("http://", "https://", "mailto:", "data:")):
            continue
        if not (path.parent / target).resolve().exists():
            bad.append(f"{path.relative_to(stage)} -> {target}")
if bad:
    print("  ✗ 发布包内存在死链：")
    for item in bad:
        print(f"    - {item}")
    sys.exit(1)
print("    链接自检通过")
PYLINK

echo "  ✓ 使用说明.md + 风险接受记录.md + 链接自检"

# 5. 入口页：告诉老师从哪开始
cat > "$STAGE/开始这里.md" <<'DOC'
# 从这里开始

## 一、打开程序

1. 进入 `ExamAdmin` 文件夹
2. Windows 双击 **`ExamAdmin.exe`**；Linux 目录版执行 **`./ExamAdmin`**。
   Linux 推荐使用另外生成的教师 **`ExamAdmin-<版本>-x86_64.AppImage`**：允许执行后双击，
   可通过「帮助 → 创建桌面启动器」添加应用菜单入口；见 `docs/arch-linux.md`。

> ⚠ 不要把主程序单独拖到别处，它需要同目录的 `_internal` 才能启动。

## 二、完整流程（约 10 分钟）

| 步骤 | 在哪 | 做什么 |
|---|---|---|
| 1 | `[题库]` 标签 | 默认已加载数学、英语与题型展示示例；正式考试请移除示例并导入自己的题库 |
| 2 | `[学生]` 标签 | 粘贴学号（每行一个），点「应用到名单」 |
| 3 | `[场次]` 标签 | 点「新建」，设考试时间，勾选科目，点「保存此场次」；需要控制难度时勾选「限制平均难度」（1–255） |
| 4 | `[打包]` 标签 | 点「生成」拿主密钥（有进度条，约 1–2 秒）→ 点「开始打包」 |
| 5 | 分发 | Windows 发生成的 `.exe`；Linux 优先发生成的 `StudentClient.AppImage`（允许执行后双击）；裸 ELF 仍可用 |
| 6 | `[收卷判分]` | 考后拖入学生交的 `.eea` → 校验 → 生成成绩 → 导出已判/未判 CSV |

> 启动和新建工程默认加载示例题库（仅供演示），学生与场次仍为空；要接着上次做，点工具栏【打开上次工程】，不会额外混入示例。
> 勾选「每生 PIN 登录」时，PIN 表在打包成功弹窗里显示路径（工程保存位置，不在考试包目录）。

**先用默认示例跑通一次**，再换成自己的题库。包根的 `示例题库/` 是可手动导入的示例（含图片题），不是新工程自动载入的内置资源。

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
ZIP="ExamAdmin-$VER-$PLATFORM.zip"
rm -f "$ZIP"
if [ "$PLATFORM" = windows ] && command -v powershell.exe >/dev/null 2>&1; then
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
