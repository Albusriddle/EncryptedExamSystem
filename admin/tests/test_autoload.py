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

        proj = out.with_suffix(".eesproj")
        assert proj.is_file(), "打包后应在同目录生成 .eesproj"
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
