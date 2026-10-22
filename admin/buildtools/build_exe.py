"""把管理端打成单文件 exe（PyInstaller）。

## 为什么要自己写脚本而不用命令行

1. `exam-builder.exe` 必须作为**资源**嵌进去，否则教师还得单独拿到它；
2. 打包前要校验 exam-builder 存在且能跑，否则打出来的 exe 一用就报错；
3. 产出后要自检：启动一次，确认 --version 正常。

用法：
    uv run python buildtools/build_exe.py
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
DIST = ROOT / "dist"


def find_artifact(name: str, root: Path | None = None) -> Path | None:
    """在 target 下找 cargo 产物。

    cargo 产物位置取决于是否指定了 `--target`：
      - `cargo build --release`              → target/release/
      - `cargo build --release --target XYZ` → target/XYZ/release/

    GitHub Actions 的 release 工作流用的是后者。若只认前者，
    CI 发布会因"找不到 exam-builder"直接失败。这里按优先级搜索。
    """
    base = root if root is not None else ROOT
    candidates = [
        base / "target" / "release" / name,  # 本机默认构建
        base / "target" / "x86_64-pc-windows-msvc" / "release" / name,
    ]
    for d in sorted((base / "target").glob("*/release")):
        candidates.append(d / name)
    for c in candidates:
        if c.is_file():
            return c
    return None


BUILDER = find_artifact(f"exam-builder{EXE}")


def main() -> int:
    if BUILDER is None:
        print("✗ 找不到 exam-builder 可执行文件")
        print("  请先运行：cargo build --release -p builder")
        print("  （已搜索 target/release 与 target/*/release）")
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
        # 版本号文件：两端水印都读它，必须打进包里，
        # 否则冻结后 app_version() 只能返回 0.0.0。
        "--add-data",
        f"{ROOT / 'VERSION'}{sep}.",
        # 品牌信息：欢迎弹窗的仓库地址/许可证/致谢都从这里读
        "--add-data",
        f"{ROOT / 'branding.json'}{sep}.",
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

    # 构建后自检：跑一次 --selftest，确认打的包里依赖齐全。
    # 没有这一步的话，"PyInstaller 成功但 exe 一启动就崩"要等到
    # 用户下载后才发现（本轮 CI 真实踩到：漏装 pydantic）。
    try:
        chk = subprocess.run(
            [str(out), "--selftest"],
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=120,
        )
    except (OSError, subprocess.SubprocessError) as e:
        print(f"✗ 自检无法运行：{e}")
        return 1
    if chk.returncode != 0:
        print("✗ 自检失败：打出来的程序无法启动，缺少依赖？")
        print((chk.stdout or "")[-800:])
        print((chk.stderr or "")[-800:])
        return 1
    print("✓ 自检通过（--selftest 成功）")

    # 版本号必须能从包里读到，否则水印会显示 0.0.0
    want = (ROOT / "VERSION").read_text(encoding="utf-8").strip()
    got = ""
    try:
        vp = subprocess.run(
            [str(out), "--version"],
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=60,
        )
        got = (vp.stdout or "").strip()
    except (OSError, subprocess.SubprocessError):
        pass
    if want and want not in got:
        print(f"✗ 版本号未打进包：期望 {want}，实际 {got or '(空)'}")
        return 1
    print(f"✓ 版本号已打包：{got}")

    # 学生端模板与手册放进同一目录：教师只需拷贝这一个文件夹
    rel = out.parent
    src = find_artifact(f"exam-client{EXE}")
    if src is not None:
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
