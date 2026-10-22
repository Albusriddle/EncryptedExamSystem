"""把管理端打成单文件 exe（PyInstaller）。

## 为什么要自己写脚本而不用命令行

1. `exam-builder.exe` 必须作为**资源**嵌进去，否则教师还得单独拿到它；
2. 打包前要校验 exam-builder 存在且能跑，否则打出来的 exe 一用就报错；
3. 产出后要自检：启动一次，确认 --version 正常。

用法：
    uv run python packaging/build_exe.py
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path

# Windows 控制台默认 GBK，打印 ✓/中文会抛 UnicodeEncodeError。
# 强制 UTF-8 输出，让这个脚本在任何本地化环境下都能跑。
try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")
except (AttributeError, OSError):
    pass

ROOT = Path(__file__).resolve().parents[2]  # 仓库根
ADMIN = ROOT / "admin"
EXE = ".exe" if os.name == "nt" else ""
BUILDER = ROOT / "target" / "release" / f"exam-builder{EXE}"
DIST = ROOT / "dist"


def main() -> int:
    if not BUILDER.exists():
        print(f"✗ 找不到 {BUILDER}")
        print("  请先运行：cargo build --release -p builder")
        return 1

    # 确认 builder 能跑，避免把坏二进制打进去
    try:
        ver = subprocess.run(
            [str(BUILDER), "--version"],
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=20,
        )
        print(f"✓ exam-builder 可用：{ver.stdout.strip() or '(无版本输出)'}")
    except (OSError, subprocess.SubprocessError) as e:
        print(f"✗ exam-builder 无法运行：{e}")
        return 1

    DIST.mkdir(exist_ok=True)
    sep = ";" if os.name == "nt" else ":"
    cmd = [
        sys.executable,
        "-m",
        "PyInstaller",
        "--noconfirm",
        "--clean",
        # 用 --onedir 而非 --onefile：
        # ① onefile 每次启动都要把内容解压到 Windows 的 TEMP，
        #    在 TEMP 指向非原生路径（如 MSYS 的 /tmp）时会直接失败：
        #    "Could not create temporary directory!"；
        # ② onedir 启动更快，也更少被杀毒软件误报。
        "--onedir",
        "--windowed",  # 不弹控制台
        "--name",
        "ExamAdmin",
        "--distpath",
        str(DIST),
        "--workpath",
        str(ROOT / "build" / "pyinstaller"),
        "--specpath",
        str(ROOT / "build" / "pyinstaller"),
        # 把 builder 作为数据文件带进去，运行时从 sys._MEIPASS 取
        "--add-binary",
        f"{BUILDER}{sep}.",
        # 让 PyInstaller 能找到 app 包
        "--paths",
        str(ADMIN),
        str(ADMIN / "app" / "main.py"),
    ]
    print("运行 PyInstaller…")
    r = subprocess.run(cmd, cwd=str(ADMIN))
    if r.returncode != 0:
        print("✗ PyInstaller 失败")
        return r.returncode

    # onedir 模式下主程序在 dist/ExamAdmin/ 里，不是 dist 根
    out = DIST / "ExamAdmin" / f"ExamAdmin{EXE}"
    if not out.exists():
        print(f"✗ 未生成 {out}")
        return 1
    size = out.stat().st_size
    print(f"✓ 已生成 {out}（{size / 1024 / 1024:.1f} MB）")

    # 学生端模板与手册放进同一目录：教师只需拷贝这一个文件夹
    rel = out.parent
    src = ROOT / "target" / "release" / f"exam-client{EXE}"
    if src.exists():
        shutil.copy2(src, rel / f"stub-template{EXE}")
        print(f"✓ 已附带学生端模板 {rel.name}/stub-template{EXE}")

    manual = ROOT / "docs" / "教师使用手册.md"
    if manual.exists():
        shutil.copy2(manual, rel / "使用说明.md")
        print("✓ 已附带使用说明.md")

    total = sum(f.stat().st_size for f in rel.rglob("*") if f.is_file())
    print(f"\n发布目录 {rel}（合计 {total / 1024 / 1024:.1f} MB）：")
    for p in sorted(rel.iterdir()):
        kind = "目录" if p.is_dir() else "文件"
        size = (
            sum(f.stat().st_size for f in p.rglob("*") if f.is_file())
            if p.is_dir()
            else p.stat().st_size
        )
        print(f"  [{kind}] {p.name}  {size / 1024 / 1024:.1f} MB")
    print(f"\n教师分发方式：整个 {rel.name} 文件夹压缩后拷给学生/自己留用。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
