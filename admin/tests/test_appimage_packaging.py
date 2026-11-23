"""AppDir 内容与下载完整性；真实 payload/网络时间门禁由 smoke-appimage.py 覆盖。"""

from __future__ import annotations

import runpy
import subprocess
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


def test_corrupted_cached_runtime_is_never_used(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(build_appimage, "ROOT", tmp_path)
    cache = tmp_path / "target/appimage-tools/runtime-x86_64"
    cache.parent.mkdir(parents=True)
    cache.write_bytes(b"corrupt runtime")
    with pytest.raises(ValueError, match="校验失败"):
        build_appimage.runtime_file()
