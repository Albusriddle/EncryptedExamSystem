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
import tempfile
import urllib.request
from collections.abc import Callable
from pathlib import Path

from app.core.desktop import DESKTOP_ID, STUDENT_DESKTOP_ID
from buildtools.build_exe import copy_legal_files, find_artifact
from buildtools.linux_resources import font_sources

ROOT = Path(__file__).resolve().parents[2]
# 固定发布版本和 SHA-256；不能直接执行会不断变动的 continuous 下载物。
RUNTIME_URL = "https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64"
RUNTIME_SHA256 = "2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d"
RUNTIME_MAX_BYTES = 4 * 1024 * 1024


def _runtime_digest(
    read: Callable[[int], bytes], write: Callable[[bytes], int] | None = None
) -> str:
    # 不信任 Content-Length；最多多读一个字节判断是否超限，且不写入临时文件。
    digest = hashlib.sha256()
    remaining = RUNTIME_MAX_BYTES
    while chunk := read(min(64 * 1024, remaining + 1)):
        remaining -= len(chunk)
        if remaining < 0:
            raise ValueError("AppImage runtime 超过大小上限，请检查固定的下载源后重试。")
        digest.update(chunk)
        if write is not None:
            write(chunk)
    return digest.hexdigest()


def runtime_file() -> Path:
    cache = ROOT / "target/appimage-tools/runtime-x86_64"
    cache.parent.mkdir(parents=True, exist_ok=True)
    try:
        if cache.is_dir():
            shutil.rmtree(cache)
        with cache.open("rb") as stream:
            if _runtime_digest(stream.read) == RUNTIME_SHA256:
                return cache
    except (OSError, ValueError):
        pass  # 无缓存、不可读、是目录或已超限，均重新下载。

    # 坏缓存由下一次成功下载修复。同目录的唯一临时文件允许并发下载，
    # 只有校验通过的文件才能原子替换缓存；失败后不会留下新坏缓存。
    tmp: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            dir=cache.parent, suffix=".download", delete=False
        ) as stream:
            tmp = Path(stream.name)
            with urllib.request.urlopen(RUNTIME_URL, timeout=60) as response:
                digest = _runtime_digest(response.read, stream.write)
        if digest != RUNTIME_SHA256:
            raise ValueError("AppImage runtime SHA-256 校验失败，请检查固定的下载源后重试。")
        tmp.replace(cache)
    finally:
        if tmp is not None:
            tmp.unlink(missing_ok=True)
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


def _student_marker(client: Path, version: str) -> bytes:
    data = client.read_bytes()
    if not data.startswith(b"\x7fELF"):
        raise ValueError("学生模板不是 Linux ELF，请在 Linux 重新构建。")
    marker = next((tag for tag in (b"EXAMKEY:EMBEDDED", b"EXAMKEY:PLAIN") if tag in data), None)
    if marker is None:
        raise ValueError("学生模板缺少身份标记，请使用本项目构建的 exam-client。")
    # 本地与带 --target 的 Cargo 产物可能同时存在。写出 AppImage 前先核对版本，
    # 不能只检查 ELF 魔数或等到成品能运行时才发现版本不对。
    rebuild = "请运行 cargo build --locked --release -p client 后重试。"
    try:
        result = subprocess.run(
            [str(client), "--version"],
            capture_output=True,
            encoding="utf-8",
            errors="replace",
            timeout=20,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        raise ValueError(f"无法运行学生模板 {client}：{exc}。{rebuild}") from exc
    reported = result.stdout.strip()
    if result.returncode or reported != f"exam-client {version}":
        raise ValueError(
            f"学生模板需要 exam-client {version}，实际为 {reported[:160] or '无版本输出'}"
            f"（退出码 {result.returncode}）。{rebuild}"
        )
    return marker


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
    client = client.resolve()
    version = (ROOT / "VERSION").read_text(encoding="utf-8").strip()
    try:
        marker = _student_marker(client, version)
    except ValueError as exc:
        parser.error(str(exc))
    runtime = runtime_file()
    output = args.output_dir.resolve()
    stage_root = ROOT / "build/appimage"
    student = prepare_appdir(stage_root / "student.AppDir", student=True)
    binary = student / "usr/bin/exam-client"
    binary.parent.mkdir(parents=True)
    shutil.copy2(client, binary)
    binary.chmod(0o755)
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
