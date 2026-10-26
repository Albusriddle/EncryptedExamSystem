"""启动空白工程 + 打开上次工程 + 打包自动保存。"""

from __future__ import annotations

from pathlib import Path

import pytest

pytest.importorskip("PySide6")

from PySide6.QtWidgets import QFileDialog, QMessageBox  # noqa: E402

from app.core.state import ProjectState  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
WORK = ROOT / "target" / "test-autoload"


@pytest.fixture(scope="module")
def qapp():
    import sys

    from PySide6.QtWidgets import QApplication

    app = QApplication.instance() or QApplication(sys.argv)
    yield app


@pytest.fixture()
def local_appdata(monkeypatch):
    import shutil

    shutil.rmtree(WORK, ignore_errors=True)
    WORK.mkdir(parents=True, exist_ok=True)
    monkeypatch.setenv("LOCALAPPDATA", str(WORK))
    yield WORK


def _mute(monkeypatch) -> None:
    monkeypatch.setattr(QMessageBox, "information", staticmethod(lambda *a, **k: None))
    monkeypatch.setattr(QMessageBox, "warning", staticmethod(lambda *a, **k: None))
    monkeypatch.setattr(QMessageBox, "critical", staticmethod(lambda *a, **k: None))
    monkeypatch.setattr(QMessageBox, "exec", lambda self: 0, raising=False)


def test_startup_is_blank_even_with_last_project(qapp, local_appdata, monkeypatch) -> None:
    from app.ui.app_settings import LAST_PROJECT, settings
    from app.ui.main_window import MainWindow

    _mute(monkeypatch)
    proj = WORK / "old.eesproj"
    ProjectState(salt="00" * 16, students=[7]).save(proj)
    st = settings()
    st.setValue(LAST_PROJECT, str(proj))
    st.sync()

    win = MainWindow()
    try:
        assert win.project_path is None, "启动必须是空白工程"
        assert win.act_last.isEnabled(), "有上次工程时按钮应可用"

        win.open_last_project()
        assert win.project_path == proj
        assert win.state.students == [7]
    finally:
        win.deleteLater()


def test_open_last_project_with_missing_file(qapp, local_appdata, monkeypatch) -> None:
    from app.ui.app_settings import LAST_PROJECT, settings
    from app.ui.main_window import MainWindow

    _mute(monkeypatch)
    st = settings()
    st.setValue(LAST_PROJECT, str(WORK / "nope.eesproj"))
    st.sync()

    win = MainWindow()
    try:
        assert not win.act_last.isEnabled()
        win.open_last_project()  # 不应崩溃
        assert win.project_path is None
    finally:
        win.deleteLater()


def test_autosave_saves_project_and_last_project(qapp, local_appdata, monkeypatch) -> None:
    from app.ui.app_settings import LAST_PROJECT, PROJECT_DIR, settings
    from app.ui.main_window import MainWindow

    _mute(monkeypatch)
    projdir = WORK / "projdir"
    settings().setValue(PROJECT_DIR, str(projdir))

    win = MainWindow()
    try:
        out = WORK / "包.exe"
        out.write_bytes(b"x")
        win._on_built(str(out))

        assert not out.with_suffix(".eesproj").exists(), "不得把工程写进分发目录"
        found = list(projdir.glob("包-*.eesproj"))
        assert found, "应存到工程保存位置"
        proj = found[0]
        assert settings().value(LAST_PROJECT, "", type=str) == str(proj)

        # 下次启动是空白；点"打开上次工程"能读回
        win2 = MainWindow()
        try:
            assert win2.project_path is None
            win2.open_last_project()
            assert win2.project_path == proj
        finally:
            win2.deleteLater()
    finally:
        win.deleteLater()


def test_same_content_builds_share_project_file(qapp, local_appdata, monkeypatch) -> None:
    from app.ui.app_settings import PROJECT_DIR, settings
    from app.ui.main_window import MainWindow

    _mute(monkeypatch)
    settings().setValue(PROJECT_DIR, str(WORK / "projdir"))

    win = MainWindow()
    try:
        out1 = WORK / "a" / "期末.exe"
        out2 = WORK / "b" / "期末.exe"
        out1.parent.mkdir(parents=True, exist_ok=True)
        out2.parent.mkdir(parents=True, exist_ok=True)
        out1.write_bytes(b"x")
        out2.write_bytes(b"x")

        win._on_built(str(out1))
        win.project_path = None
        win._on_built(str(out2))

        files = list((WORK / "projdir").glob("期末-*.eesproj"))
        assert len(files) == 1, f"同内容不应产生多份工程：{files}"
    finally:
        win.deleteLater()


def test_projects_dir_uses_program_dir_when_frozen(monkeypatch, tmp_path) -> None:
    """打包后工程应存在 ExamAdmin.exe 同级的 projects/，而不是 appdata。"""
    import sys

    from app.ui import app_settings

    fake_exe = tmp_path / "ExamAdmin" / "ExamAdmin.exe"
    fake_exe.parent.mkdir(parents=True)
    fake_exe.write_bytes(b"x")
    monkeypatch.setattr(sys, "frozen", True, raising=False)
    monkeypatch.setattr(sys, "executable", str(fake_exe))

    got = app_settings.projects_dir()
    assert got == fake_exe.parent / "projects"
    assert got.is_dir()


def test_open_project_updates_last_project_and_build_dir(qapp, local_appdata, monkeypatch) -> None:
    from app.ui.app_settings import LAST_PROJECT, PROJECT_DIR, settings
    from app.ui.main_window import MainWindow

    _mute(monkeypatch)
    proj = WORK / "opened" / "自定义工程.eesproj"
    proj.parent.mkdir(parents=True, exist_ok=True)
    ProjectState(salt="11" * 16, students=[3]).save(proj)
    st = settings()
    st.setValue(LAST_PROJECT, str(WORK / "old.eesproj"))
    st.sync()

    monkeypatch.setattr(
        QFileDialog, "getOpenFileName", staticmethod(lambda *a, **k: (str(proj), ""))
    )
    win = MainWindow()
    try:
        win.open_project()
        assert win.project_path == proj
        assert settings().value(LAST_PROJECT, "", type=str) == str(proj)
        assert settings().value(PROJECT_DIR, "", type=str) == str(proj.parent)
        assert win.build_tab.edit_projdir.text() == str(proj.parent)
    finally:
        win.deleteLater()


def test_changed_project_dir_saves_copy(qapp, local_appdata, monkeypatch) -> None:
    from app.ui.app_settings import PROJECT_DIR, settings
    from app.ui.main_window import MainWindow

    _mute(monkeypatch)
    old_dir = WORK / "old"
    new_dir = WORK / "new"
    settings().setValue(PROJECT_DIR, str(old_dir))

    win = MainWindow()
    try:
        out = WORK / "包.exe"
        out.write_bytes(b"x")
        win._on_built(str(out))
        old_proj = win.project_path
        assert old_proj is not None and old_proj.parent == old_dir

        win.build_tab.edit_projdir.setText(str(new_dir))
        win._on_built(str(out))

        assert win.project_path is not None
        assert win.project_path.parent == new_dir
        assert win.project_path.name == old_proj.name
        assert old_proj.exists(), "旧工程应保留"
        assert settings().value(PROJECT_DIR, "", type=str) == str(new_dir)
    finally:
        win.deleteLater()
