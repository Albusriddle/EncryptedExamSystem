"""构建教师与学生 AppImage；考试数据仍由原 builder 追加，不重新实现加密。

用法（先运行 build_exe.py）：uv run python -m buildtools.build_appimage
只构建学生模板：uv run python -m buildtools.build_appimage --student-only
"""

from __future__ import annotations

import argparse
import hashlib
import json
import platform
import shutil
import subprocess
import sys
import urllib.request
from pathlib import Path

from app.core.desktop import DESKTOP_ID, STUDENT_DESKTOP_ID
from buildtools.build_exe import copy_legal_files, find_artifact
from buildtools.linux_resources import font_sources

ROOT = Path(__file__).resolve().parents[2]
# 固定发布版本和 SHA-256；不能直接执行会不断变动的 continuous 下载物。
RUNTIME_URL = "https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64"
RUNTIME_SHA256 = "2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d"


def runtime_file() -> Path:
    cache = ROOT / "target/appimage-tools/runtime-x86_64"
    cache.parent.mkdir(parents=True, exist_ok=True)
    if not cache.is_file():
        tmp = cache.with_suffix(".download")
        try:
            with urllib.request.urlopen(RUNTIME_URL, timeout=60) as response:
                with tmp.open("wb") as stream:
                    shutil.copyfileobj(response, stream)
            tmp.replace(cache)
        finally:
            tmp.unlink(missing_ok=True)
    digest = hashlib.sha256(cache.read_bytes()).hexdigest()
    if digest != RUNTIME_SHA256:
        raise ValueError(f"AppImage runtime 校验失败，请删除 {cache} 后重新构建。")
    return cache


def prepare_appdir(stage: Path, *, student: bool) -> Path:
    if stage.exists():
        shutil.rmtree(stage)
    stage.mkdir(parents=True)
    app_id = STUDENT_DESKTOP_ID if student else DESKTOP_ID
    product = json.loads((ROOT / "branding.json").read_text(encoding="utf-8"))["product"]
    name = f"{product} · {'学生端' if student else '管理端'}"
    (stage / f"{app_id}.desktop").write_text(
        "[Desktop Entry]\nType=Application\n"
        f"Name={name}\nExec={'exam-client' if student else 'ExamAdmin'}\n"
        f"Icon={app_id}\nStartupWMClass={app_id}\n"
        "Terminal=false\nCategories=Education;\n",
        encoding="utf-8",
    )
    shutil.copy2(ROOT / "assets/ees.svg", stage / f"{app_id}.svg")
    shutil.copy2(ROOT / "assets/ees.svg", stage / ".DirIcon")
    docs = stage / "usr/share/doc/EncryptedExamSystem"
    docs.mkdir(parents=True)
    copy_legal_files(docs)
    font_dir = stage / "usr/share/fonts"
    font_dir.mkdir(parents=True)
    for font in font_sources():
        shutil.copy2(font, font_dir / font.name)
    executable = "usr/bin/exam-client" if student else "usr/lib/ExamAdmin/ExamAdmin"
    launcher = stage / "AppRun"
    launcher.write_text(
        "#!/bin/sh\nset -eu\n"
        'HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)\n'
        'export APPDIR="$HERE"\n'
        # 防止系统 Qt/Conda 插件污染随包的 Qt/Python；保留用户的 backend/DPI 选择。
        "unset QT_PLUGIN_PATH QT_QPA_PLATFORM_PLUGIN_PATH QML2_IMPORT_PATH PYTHONHOME PYTHONPATH\n"
        f'exec "$HERE/{executable}" "$@"\n',
        encoding="utf-8",
    )
    launcher.chmod(0o755)
    return stage


def make_image(stage: Path, out: Path, runtime: Path, marker: bytes = b"") -> None:
    filesystem = stage.parent / f"{stage.name}.squashfs"
    subprocess.run(
        [
            "mksquashfs",
            str(stage),
            str(filesystem),
            "-noappend",
            "-all-root",
            "-no-xattrs",
            "-no-progress",
            "-comp",
            "zstd",
            "-processors",
            "2",
        ],
        check=True,
        encoding="utf-8",
    )
    out.parent.mkdir(parents=True, exist_ok=True)
    tmp = out.with_suffix(".AppImage.tmp")
    try:
        with tmp.open("wb") as stream:
            for source in (runtime, filesystem):
                with source.open("rb") as src:
                    shutil.copyfileobj(src, stream)
            # SquashFS 压缩后 builder 看不到原 ELF 的身份标记；放一份公开状态标记。
            # 这里只是模板身份，不含 master 或 K_manifest。考试 payload 由 builder 追加。
            stream.write(marker)
        tmp.chmod(0o755)
        tmp.replace(out)
    finally:
        tmp.unlink(missing_ok=True)
    print(f"✓ {out} ({out.stat().st_size / 1048576:.1f} MiB)")


def check_image(path: Path, *args: str) -> None:
    result = subprocess.run(
        [str(path), "--appimage-extract-and-run", *args],
        capture_output=True,
        encoding="utf-8",
        errors="replace",
        timeout=180,
    )
    if result.returncode:
        raise RuntimeError(f"AppImage 自检失败：{path.name}\n{result.stderr[-2000:]}")
    print(f"✓ {path.name} {' '.join(args)}: {result.stdout.strip() or 'OK'}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--student-only", action="store_true")
    parser.add_argument("--client", type=Path)
    parser.add_argument("--output-dir", type=Path, default=ROOT / "dist")
    args = parser.parse_args()
    if sys.platform != "linux" or platform.machine() != "x86_64":
        parser.error("目前只支持原生 Linux x86_64 构建，不交叉打包 Windows exe。")
    if not shutil.which("mksquashfs"):
        parser.error("缺少 mksquashfs；请安装 squashfs-tools。")
    client = args.client or find_artifact("exam-client")
    if client is None or not client.is_file():
        parser.error("缺少学生端：先 cargo build --locked --release --workspace。")
    if client.read_bytes()[:4] != b"\x7fELF":
        parser.error("学生模板不是 Linux ELF，请在 Linux 重新构建。")
    runtime = runtime_file()
    output = args.output_dir.resolve()
    stage_root = ROOT / "build/appimage"
    student = prepare_appdir(stage_root / "student.AppDir", student=True)
    binary = student / "usr/bin/exam-client"
    binary.parent.mkdir(parents=True)
    shutil.copy2(client, binary)
    binary.chmod(0o755)
    marker = next(
        (
            value
            for value in (b"EXAMKEY:EMBEDDED", b"EXAMKEY:PLAIN")
            if value in client.read_bytes()
        ),
        None,
    )
    if marker is None:
        parser.error("学生模板缺少身份标记，请使用本项目构建的 exam-client。")
    stub = output / "stub-template-x86_64.AppImage"
    make_image(student, stub, runtime, marker)
    check_image(stub, "--version")
    if args.student_only:
        return 0
    admin = ROOT / "dist/ExamAdmin"
    if not (admin / "ExamAdmin").is_file():
        parser.error("缺少教师端发布目录，先运行 buildtools/build_exe.py。")
    shutil.copy2(stub, admin / "stub-template.AppImage")
    teacher = prepare_appdir(stage_root / "teacher.AppDir", student=False)
    shutil.copytree(admin, teacher / "usr/lib/ExamAdmin", dirs_exist_ok=True)
    version = (ROOT / "VERSION").read_text(encoding="utf-8").strip()
    image = output / f"ExamAdmin-{version}-x86_64.AppImage"
    make_image(teacher, image, runtime)
    check_image(image, "--selftest")
    check_image(image, "--version")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as exc:
        print(f"✗ {exc}", file=sys.stderr)
        raise SystemExit(1) from None
