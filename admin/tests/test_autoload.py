"""打包自动保存工程 + 启动自动打开。"""

from __future__ import annotations

from pathlib import Path

import pytest

pytest.importorskip("PySide6")

from PySide6.QtWidgets import QMessageBox  # noqa: E402

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

    shutil.rmtree(WORK, ignore_errors=True)  # 上一轮留下的 .eesproj 会让断言误判
    WORK.mkdir(parents=True, exist_ok=True)
    monkeypatch.setenv("LOCALAPPDATA", str(WORK))
    yield WORK


def test_autosave_and_autoload_roundtrip(qapp, local_appdata, monkeypatch) -> None:
    from app.ui.app_settings import AUTOLOAD, LAST_PROJECT, settings
    from app.ui.main_window import MainWindow

    monkeypatch.setattr(QMessageBox, "information", staticmethod(lambda *a, **k: None))
    monkeypatch.setattr(QMessageBox, "warning", staticmethod(lambda *a, **k: None))

    st = settings()
    st.setValue(AUTOLOAD, True)
    st.sync()

    win = MainWindow()
    try:
        out = WORK / "包.exe"
        out.write_bytes(b"x")
        win._on_built(str(out))

        # 绝不能写在考试包旁边（那是发给学生的目录）
        assert not out.with_suffix(".eesproj").exists(), "不得把工程写进分发目录"
        projects = WORK / "ExamSystem" / "projects"
        found = list(projects.glob("包-*.eesproj"))
        assert found, "应存到教师端专属 projects 目录（带短哈希后缀）"
        proj = found[0]
        assert settings().value(LAST_PROJECT, "", type=str) == str(proj)

        # 再开一个主窗口：应自动打开刚才的工程
        win2 = MainWindow()
        try:
            assert win2.project_path == proj
            assert win2.state.salt == win.state.salt
        finally:
            win2.deleteLater()
    finally:
        win.deleteLater()


def test_autoload_can_be_disabled(qapp, local_appdata, monkeypatch) -> None:
    from app.ui.app_settings import AUTOLOAD, LAST_PROJECT, settings
    from app.ui.main_window import MainWindow

    proj = WORK / "p.eesproj"
    ProjectState(salt="11" * 16, students=[7]).save(proj)
    st = settings()
    st.setValue(LAST_PROJECT, str(proj))
    st.setValue(AUTOLOAD, False)
    st.sync()

    win = MainWindow()
    try:
        assert win.project_path is None
        assert win.state.students == []
    finally:
        win.deleteLater()


def test_missing_project_does_not_crash(qapp, local_appdata) -> None:
    from app.ui.app_settings import AUTOLOAD, LAST_PROJECT, settings
    from app.ui.main_window import MainWindow

    st = settings()
    st.setValue(LAST_PROJECT, str(WORK / "nope.eesproj"))
    st.setValue(AUTOLOAD, True)
    st.sync()

    win = MainWindow()
    try:
        assert win.project_path is None
    finally:
        win.deleteLater()


def test_same_content_builds_share_project_file(qapp, local_appdata, monkeypatch) -> None:
    """同内容、不同输出路径的两个包应复用同一个 .eesproj（移动包不新建）。"""
    from app.ui.app_settings import settings
    from app.ui.main_window import MainWindow

    monkeypatch.setattr(QMessageBox, "information", staticmethod(lambda *a, **k: None))
    monkeypatch.setattr(QMessageBox, "warning", staticmethod(lambda *a, **k: None))
    settings().setValue("autoload", False)

    win = MainWindow()
    try:
        out1 = WORK / "a" / "期末.exe"
        out2 = WORK / "b" / "期末.exe"
        out1.parent.mkdir(parents=True, exist_ok=True)
        out2.parent.mkdir(parents=True, exist_ok=True)
        out1.write_bytes(b"x")
        out2.write_bytes(b"x")

        win._on_built(str(out1))
        win.project_path = None  # 模拟没有手动保存过工程
        win._on_built(str(out2))

        files = list((WORK / "ExamSystem" / "projects").glob("期末-*.eesproj"))
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
