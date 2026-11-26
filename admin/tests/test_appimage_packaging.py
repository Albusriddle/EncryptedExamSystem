"""AppDir 内容与下载完整性；真实 payload/网络时间门禁由 smoke-appimage.py 覆盖。"""

from __future__ import annotations

import hashlib
import runpy
import subprocess
import sys
from io import BytesIO
from pathlib import Path

import pytest

from buildtools import build_appimage, linux_resources


def test_missing_font_is_build_error(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(linux_resources, "FONT_DIRS", (tmp_path,))
    with pytest.raises(FileNotFoundError, match="noto-fonts-cjk"):
        linux_resources.font_sources()


def test_font_bundle_has_regular_and_bold(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    paths = [tmp_path / name for name in linux_resources.FONT_NAMES]
    for path in paths:
        path.touch()
    monkeypatch.setattr(linux_resources, "FONT_DIRS", (tmp_path,))
    assert linux_resources.font_sources() == paths


@pytest.mark.parametrize("student", [False, True])
def test_appdir_contains_desktop_icon_fonts_and_licenses(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, student: bool
) -> None:
    font = tmp_path / "font.ttc"
    font.write_bytes(b"fixture")
    monkeypatch.setattr(build_appimage, "font_sources", lambda: [font])
    stage = build_appimage.prepare_appdir(tmp_path / "app.AppDir", student=student)
    desktop_files = list(stage.glob("*.desktop"))
    assert len(desktop_files) == 1
    assert "Terminal=false" in desktop_files[0].read_text(encoding="utf-8")
    assert (stage / ".DirIcon").is_file()
    assert (stage / "usr/share/fonts/font.ttc").is_file()
    docs = stage / "usr/share/doc/EncryptedExamSystem"
    assert (docs / "licenses/OFL-1.1-Noto.txt").is_file()
    assert (docs / "licenses/AppImage-runtime.txt").is_file()
    script = (stage / "AppRun").read_text(encoding="utf-8")
    assert '"$@"' in script
    assert 'export APPDIR="$HERE"' in script
    executable = "usr/bin/exam-client" if student else "usr/lib/ExamAdmin/ExamAdmin"
    assert f'exec "$HERE/{executable}"' in script


def test_smoke_failure_never_logs_master(monkeypatch: pytest.MonkeyPatch) -> None:
    script = runpy.run_path(str(build_appimage.ROOT / "scripts/smoke-appimage.py"))
    sensitive = "synthetic-" + str(id(script))
    result = subprocess.CompletedProcess(["program"], 1, stdout=sensitive, stderr="failed")
    monkeypatch.setattr(subprocess, "run", lambda *args, **kwargs: result)
    with pytest.raises(RuntimeError) as error:
        script["run"](["program"], env={"EXAM_MASTER": sensitive})
    assert sensitive not in str(error.value)
    assert "[REDACTED]" in str(error.value)


@pytest.fixture
def runtime_cache(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    monkeypatch.setattr(build_appimage, "ROOT", tmp_path)
    return tmp_path / "target/appimage-tools/runtime-x86_64"


def test_corrupted_cached_runtime_is_never_used(
    runtime_cache: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    runtime_cache.parent.mkdir(parents=True)
    runtime_cache.write_bytes(b"corrupt cache")
    monkeypatch.setattr(
        build_appimage.urllib.request, "urlopen", lambda *_a, **_k: BytesIO(b"corrupt download")
    )
    with pytest.raises(ValueError, match="校验失败"):
        build_appimage.runtime_file()
    assert runtime_cache.read_bytes() == b"corrupt cache", "不能用另一份坏文件覆盖缓存"
    assert list(runtime_cache.parent.iterdir()) == [runtime_cache]


def test_bad_runtime_download_leaves_no_cache(
    runtime_cache: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(
        build_appimage.urllib.request, "urlopen", lambda *_a, **_k: BytesIO(b"wrong checksum")
    )
    with pytest.raises(ValueError, match="校验失败"):
        build_appimage.runtime_file()
    assert list(runtime_cache.parent.iterdir()) == []


def test_runtime_download_has_a_size_limit_even_without_content_length(
    runtime_cache: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    body = b"a" * 100
    monkeypatch.setattr(build_appimage, "RUNTIME_MAX_BYTES", 32, raising=False)
    monkeypatch.setattr(build_appimage, "RUNTIME_SHA256", hashlib.sha256(body).hexdigest())
    response = BytesIO(body)
    read_sizes = []

    def read_with_count(size: int = -1) -> bytes:
        chunk = BytesIO.read(response, size)
        read_sizes.append(len(chunk))
        return chunk

    monkeypatch.setattr(response, "read", read_with_count)
    monkeypatch.setattr(build_appimage.urllib.request, "urlopen", lambda *_a, **_k: response)
    with pytest.raises(ValueError, match="大小上限"):
        build_appimage.runtime_file()
    assert sum(read_sizes) == 33
    assert list(runtime_cache.parent.iterdir()) == []


def test_corrupted_cache_is_atomically_replaced_and_good_cache_is_reused(
    runtime_cache: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    runtime_cache.parent.mkdir(parents=True)
    runtime_cache.write_bytes(b"corrupt cache")
    good = b"verified runtime"
    monkeypatch.setattr(build_appimage, "RUNTIME_SHA256", hashlib.sha256(good).hexdigest())
    calls = []

    def download(*_a, **_k) -> BytesIO:
        calls.append(True)
        return BytesIO(good)

    monkeypatch.setattr(build_appimage.urllib.request, "urlopen", download)
    assert build_appimage.runtime_file() == runtime_cache
    assert runtime_cache.read_bytes() == good
    assert list(runtime_cache.parent.iterdir()) == [runtime_cache]
    assert build_appimage.runtime_file() == runtime_cache
    assert calls == [True]


@pytest.mark.parametrize(
    ("stdout", "returncode", "ok"),
    [
        ("exam-client 1.2.3\n", 0, True),
        ("exam-client 1.2.2\n", 0, False),
        ("exam-client 1.2.30\n", 0, False),
        ("exam-client 1.2.3\n", 1, False),
    ],
)
def test_student_appimage_requires_a_current_client(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    stdout: str,
    returncode: int,
    ok: bool,
) -> None:
    client = tmp_path / "exam-client"
    client.write_bytes(b"\x7fELF\0EXAMKEY:EMBEDDED")
    (tmp_path / "VERSION").write_text("1.2.3\n", encoding="utf-8")
    monkeypatch.setattr(sys, "argv", ["build_appimage", "--student-only", "--client", str(client)])
    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.setattr(build_appimage.platform, "machine", lambda: "x86_64")
    monkeypatch.setattr(build_appimage.shutil, "which", lambda name: name)
    monkeypatch.setattr(build_appimage, "ROOT", tmp_path)
    monkeypatch.setattr(build_appimage, "runtime_file", lambda: tmp_path / "runtime")
    monkeypatch.setattr(build_appimage, "prepare_appdir", lambda stage, **_k: stage)
    monkeypatch.setattr(build_appimage, "check_image", lambda *_a: None)

    def probe(cmd: list[str], *, encoding: str, **_k) -> subprocess.CompletedProcess[str]:
        assert cmd == [str(client.resolve()), "--version"]
        assert encoding == "utf-8"
        return subprocess.CompletedProcess(cmd, returncode, stdout=stdout, stderr="")

    def make_image(_stage: Path, output: Path, _runtime: Path, _marker: bytes) -> None:
        output.parent.mkdir()
        output.touch()

    monkeypatch.setattr(build_appimage.subprocess, "run", probe)
    monkeypatch.setattr(build_appimage, "make_image", make_image)
    stub = tmp_path / "dist" / "stub-template-x86_64.AppImage"
    if ok:
        assert build_appimage.main() == 0
        assert stub.is_file()
    else:
        with pytest.raises(SystemExit, match="2"):
            build_appimage.main()
        message = capsys.readouterr().err
        assert "学生模板" in message and "1.2.3" in message and "cargo build" in message
        assert not stub.exists()
