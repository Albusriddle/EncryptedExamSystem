"""AppImage 必须保留 Windows 的双击打包流程，用户文件不能落进临时挂载点。"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

pytest.importorskip("PySide6")

from PySide6.QtWidgets import QApplication, QMessageBox  # noqa: E402

from app.core.state import ProjectState  # noqa: E402
from app.ui import app_settings, tab_build  # noqa: E402


@pytest.fixture(scope="module")
def qapp():
    return QApplication.instance() or QApplication([])


@pytest.fixture
def image(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> tuple[Path, Path]:
    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.setattr(sys, "frozen", True, raising=False)
    outer = tmp_path / "下载" / "ExamAdmin.AppImage"
    outer.parent.mkdir()
    outer.touch()
    mount = tmp_path / ".mount_ExamAdmin"
    inner = mount / "usr/lib/ExamAdmin/ExamAdmin"
    inner.parent.mkdir(parents=True)
    inner.touch()
    monkeypatch.setenv("APPIMAGE", str(outer))
    monkeypatch.setenv("APPDIR", str(mount))
    monkeypatch.setattr(sys, "executable", str(inner))
    return outer, inner


def test_appimage_build_tab_defaults_to_portable_student(qapp, image) -> None:
    outer, inner = image
    (inner.parent / "stub-template").touch()
    portable = inner.parent / "stub-template.AppImage"
    portable.touch()
    tab = tab_build.BuildTab(ProjectState())
    assert tab.edit_stub.text() == str(portable)
    assert Path(tab.edit_out.text()).name == "StudentClient.AppImage"
    assert not Path(tab.edit_out.text()).is_relative_to(inner.parent)
    tab.deleteLater()


def test_appimage_help_action_creates_launcher_only_on_click(qapp, image, monkeypatch) -> None:
    from app.core import desktop
    from app.ui.main_window import MainWindow

    calls = []
    messages = []
    destination = image[0].parent / "test.desktop"

    def install() -> Path:
        calls.append(True)
        return destination

    monkeypatch.setattr(desktop, "install_desktop_entry", install)
    monkeypatch.setattr(QMessageBox, "information", lambda *args: messages.append(args[2]))
    win = MainWindow()
    menu = win.menuBar().actions()[0].menu()
    action = next(a for a in menu.actions() if a.text() == "创建桌面启动器…")
    assert not calls, "启动程序不应自动写入桌面菜单"
    action.trigger()
    assert calls == [True]
    assert messages and str(destination) in messages[0]
    win.deleteLater()


def test_appimage_projects_use_persistent_distribution_directory(image) -> None:
    outer, inner = image
    assert app_settings.teacher_dir() == outer.parent
    directory = app_settings.projects_dir()
    assert directory == outer.parent / "projects"
    assert not directory.is_relative_to(inner.parent)
    (directory / "saved-project.txt").write_text("persistent", encoding="utf-8")


def test_stale_appimage_environment_does_not_redirect_native_app(image, monkeypatch) -> None:
    outer, inner = image
    native = outer.parent / "native/ExamAdmin"
    monkeypatch.setattr(sys, "executable", str(native))
    assert app_settings.teacher_dir() == native.parent


def test_appimage_default_output_remains_native_without_portable_template(image) -> None:
    outer, inner = image
    (inner.parent / "stub-template").touch()
    assert tab_build.default_stub() == inner.parent / "stub-template"
    assert tab_build.default_out().name == "StudentClient"
