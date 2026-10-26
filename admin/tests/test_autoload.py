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
        out = WORK / "out" / "包.exe"
        out.parent.mkdir(parents=True, exist_ok=True)
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
        out = WORK / "out" / "包.exe"
        out.parent.mkdir(parents=True, exist_ok=True)
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


def test_project_dir_is_normalized_to_absolute(qapp, local_appdata, monkeypatch) -> None:
    from app.ui.app_settings import PROJECT_DIR, settings
    from app.ui.main_window import MainWindow

    _mute(monkeypatch)
    monkeypatch.chdir(WORK)
    win = MainWindow()
    try:
        win.build_tab.edit_projdir.setText("相对目录")
        win.build_tab.commit_project_dir()
        saved = Path(settings().value(PROJECT_DIR, "", type=str))
        assert saved.is_absolute()
        assert saved == (WORK / "相对目录").resolve()
    finally:
        win.deleteLater()


def test_save_dialog_starts_in_project_dir(qapp, local_appdata, monkeypatch) -> None:
    from app.ui.app_settings import PROJECT_DIR, settings
    from app.ui.main_window import MainWindow

    _mute(monkeypatch)
    settings().setValue(PROJECT_DIR, str(WORK / "projdir"))
    captured: list[str] = []

    def fake_save(parent, title, directory, filters):  # noqa: ANN001, ARG001
        captured.append(directory)
        return ("", "")

    monkeypatch.setattr(QFileDialog, "getSaveFileName", staticmethod(fake_save))
    win = MainWindow()
    try:
        win.save_project()
        assert captured and captured[0].startswith(str(WORK / "projdir"))
    finally:
        win.deleteLater()


def test_open_last_project_reports_missing_path(qapp, local_appdata, monkeypatch) -> None:
    from app.ui.app_settings import LAST_PROJECT, settings
    from app.ui.main_window import MainWindow

    shown: list[str] = []
    monkeypatch.setattr(
        QMessageBox, "warning", staticmethod(lambda parent, title, text: shown.append(text))
    )
    st = settings()
    st.setValue(LAST_PROJECT, str(WORK / "gone.eesproj"))
    st.sync()

    win = MainWindow()
    try:
        win.open_last_project()
        assert shown and "已不存在" in shown[0]
    finally:
        win.deleteLater()


def test_same_name_copy_does_not_overwrite(qapp, local_appdata, monkeypatch) -> None:
    from app.ui.app_settings import PROJECT_DIR, settings
    from app.ui.main_window import MainWindow

    _mute(monkeypatch)
    old_dir = WORK / "old2"
    new_dir = WORK / "new2"
    settings().setValue(PROJECT_DIR, str(old_dir))

    win = MainWindow()
    try:
        out = WORK / "out2" / "包2.exe"
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_bytes(b"x")
        win._on_built(str(out))
        old_proj = win.project_path
        assert old_proj is not None

        new_dir.mkdir(parents=True, exist_ok=True)
        conflict = new_dir / old_proj.name
        conflict.write_text("另一个工程", encoding="utf-8")

        win.build_tab.edit_projdir.setText(str(new_dir))
        win._on_built(str(out))

        assert win.project_path is not None
        assert win.project_path != conflict
        assert win.project_path.name.endswith("-1.eesproj")
        assert conflict.read_text(encoding="utf-8") == "另一个工程"
    finally:
        win.deleteLater()


def test_build_never_saves_project_into_output_dir(qapp, local_appdata, monkeypatch) -> None:
    from app.ui.app_settings import PROJECT_DIR, settings
    from app.ui.main_window import MainWindow

    _mute(monkeypatch)
    out_dir = WORK / "考试包"
    out_dir.mkdir(parents=True, exist_ok=True)
    settings().setValue(PROJECT_DIR, str(out_dir))

    win = MainWindow()
    try:
        out = out_dir / "StudentClient.exe"
        out.write_bytes(b"x")
        win._on_built(str(out))

        assert win.project_path is not None
        assert win.project_path.parent != out_dir, "工程不能写进要发给学生的目录"
        assert not list(out_dir.glob("*.eesproj"))
    finally:
        win.deleteLater()


def test_new_project_falls_back_when_dir_invalid(qapp, local_appdata, monkeypatch) -> None:
    from app.ui.app_settings import PROJECT_DIR, settings
    from app.ui.main_window import MainWindow

    _mute(monkeypatch)
    bad = WORK / "bad-dir"
    bad.write_text("not a dir", encoding="utf-8")
    settings().setValue(PROJECT_DIR, str(bad))

    win = MainWindow()
    try:
        win.new_project()
        assert settings().value(PROJECT_DIR, "", type=str) == ""
        assert win.build_tab.edit_projdir.text() == str(
            __import__("app.ui.app_settings", fromlist=["projects_dir"]).projects_dir()
        )
    finally:
        win.deleteLater()


def test_last_project_tooltip_reports_missing(qapp, local_appdata, monkeypatch) -> None:
    from app.ui.app_settings import LAST_PROJECT, settings
    from app.ui.main_window import MainWindow

    _mute(monkeypatch)
    st = settings()
    st.setValue(LAST_PROJECT, str(WORK / "gone2.eesproj"))
    st.sync()

    win = MainWindow()
    try:
        assert "已不存在" in win.act_last.toolTip()
    finally:
        win.deleteLater()


def test_build_avoids_subdir_of_output(qapp, local_appdata, monkeypatch) -> None:
    from app.ui.app_settings import PROJECT_DIR, settings
    from app.ui.main_window import MainWindow

    _mute(monkeypatch)
    out_dir = WORK / "out3"
    settings().setValue(PROJECT_DIR, str(out_dir / "工程"))

    win = MainWindow()
    try:
        out = out_dir / "包.exe"
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_bytes(b"x")
        win._on_built(str(out))
        assert not list(out_dir.rglob("*.eesproj")), "子目录也会随包发出去，必须避开"
    finally:
        win.deleteLater()


def test_fallback_also_avoids_output_dir(qapp, local_appdata, monkeypatch) -> None:
    from app.ui import main_window as mw
    from app.ui.app_settings import PROJECT_DIR, settings

    _mute(monkeypatch)
    out_dir = WORK / "out4"
    out_dir.mkdir(parents=True, exist_ok=True)
    settings().setValue(PROJECT_DIR, str(out_dir))
    # 模拟"默认目录也在输出目录里"的极端情况
    monkeypatch.setattr(mw, "projects_dir", lambda: out_dir / "projects")

    win = mw.MainWindow()
    try:
        out = out_dir / "包.exe"
        out.write_bytes(b"x")
        win._on_built(str(out))
        assert not list(out_dir.rglob("*.eesproj"))
    finally:
        win.deleteLater()


def test_stray_project_files_are_reported(qapp, local_appdata, monkeypatch) -> None:
    from app.ui.app_settings import PROJECT_DIR, settings
    from app.ui.main_window import MainWindow

    _mute(monkeypatch)
    out_dir = WORK / "out5"
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "old-a.eesproj").write_text("x", encoding="utf-8")
    (out_dir / "old-b.eesproj").write_text("x", encoding="utf-8")
    settings().setValue(PROJECT_DIR, str(WORK / "projdir5"))

    warnings: list[str] = []
    monkeypatch.setattr(
        QMessageBox, "warning", staticmethod(lambda parent, title, text: warnings.append(text))
    )

    win = MainWindow()
    try:
        out = out_dir / "包.exe"
        out.write_bytes(b"x")
        win._on_built(str(out))
        assert any("2 个工程文件" in w for w in warnings), warnings
    finally:
        win.deleteLater()


def test_save_project_warns_inside_output_dir(qapp, local_appdata, monkeypatch) -> None:
    from app.ui.app_settings import PROJECT_DIR, settings
    from app.ui.main_window import MainWindow

    _mute(monkeypatch)
    out_dir = WORK / "out6"
    out_dir.mkdir(parents=True, exist_ok=True)
    settings().setValue(PROJECT_DIR, str(WORK / "projdir6"))

    monkeypatch.setattr(
        QFileDialog,
        "getSaveFileName",
        staticmethod(lambda *a, **k: (str(out_dir / "x.eesproj"), "")),
    )
    monkeypatch.setattr(
        QMessageBox, "question", staticmethod(lambda *a, **k: QMessageBox.StandardButton.No)
    )

    win = MainWindow()
    try:
        win.build_tab.edit_out.setText(str(out_dir / "StudentClient.exe"))
        win.save_project()
        assert not (out_dir / "x.eesproj").exists()
    finally:
        win.deleteLater()
