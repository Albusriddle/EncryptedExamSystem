"""原生 Linux 打包、收卷与网页考试的路径回归。"""

from __future__ import annotations

import runpy
import sys
from pathlib import Path

import pytest

pytest.importorskip("PySide6")

from PySide6.QtWidgets import QApplication, QFileDialog  # noqa: E402

from app.core.state import ProjectState  # noqa: E402
from app.ui import tab_answers, tab_build, web_exam_dialog  # noqa: E402


@pytest.fixture(scope="module")
def qapp():
    return QApplication.instance() or QApplication([])


@pytest.fixture(autouse=True)
def linux_only() -> None:
    if sys.platform != "linux":
        pytest.skip("Linux 原生路径；Windows 由现有用例覆盖")


def test_module_entrypoint(monkeypatch: pytest.MonkeyPatch) -> None:
    import app.main

    monkeypatch.setattr(app.main, "main", lambda: 0)
    with pytest.raises(SystemExit) as exc:
        runpy.run_module("app", run_name="__main__")
    assert exc.value.code == 0


def test_default_linux_output_has_no_exe_suffix(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(sys, "frozen", False, raising=False)
    assert tab_build.default_out().name == "StudentClient"


def test_save_dialog_accepts_extensionless_package(qapp, monkeypatch: pytest.MonkeyPatch) -> None:
    seen = []

    def pick(*args):
        seen.append(args[-1])
        return "", ""

    monkeypatch.setattr(QFileDialog, "getSaveFileName", pick)
    tab = tab_build.BuildTab(ProjectState())
    tab._pick_save()
    assert seen and seen[0].split(";;")[0].endswith("(*)")
    tab.deleteLater()


def test_answer_dialog_selects_extensionless_package(
    qapp, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    package = tmp_path / "StudentClient"
    seen = []

    def pick(*args):
        seen.append(args[-1])
        return str(package), ""

    monkeypatch.setattr(QFileDialog, "getOpenFileName", pick)
    tab = tab_answers.AnswersTab(ProjectState())
    tab._pick_package()
    assert seen and seen[0].split(";;")[0].endswith("(*)")
    assert tab.pkg.text() == str(package)
    tab.deleteLater()


@pytest.mark.parametrize("frozen", [False, True])
def test_web_dialog_finds_native_server(
    qapp, tmp_path: Path, monkeypatch: pytest.MonkeyPatch, frozen: bool
) -> None:
    root = tmp_path / "源码"
    monkeypatch.setattr(
        web_exam_dialog, "__file__", str(root / "admin" / "app" / "ui" / "web_exam_dialog.py")
    )
    monkeypatch.delattr(sys, "_MEIPASS", raising=False)
    if frozen:
        base = tmp_path / "ExamAdmin" / "_internal"
        monkeypatch.setattr(sys, "_MEIPASS", str(base), raising=False)
    else:
        base = root / "target" / "release"
    base.mkdir(parents=True)
    server = base / "exam-server"
    server.write_bytes(b"native server")
    dialog = web_exam_dialog.WebExamDialog(None, "", "")
    assert dialog._server_path() == server
    dialog.deleteLater()


def test_crash_log_uses_xdg_state_home(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    from app.main import log_dir

    monkeypatch.delenv("LOCALAPPDATA", raising=False)
    monkeypatch.delenv("APPDATA", raising=False)
    monkeypatch.setenv("XDG_STATE_HOME", str(tmp_path / "state"))
    assert log_dir() == tmp_path / "state" / "ExamSystem"
    assert log_dir().is_dir()
