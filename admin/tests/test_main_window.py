"""主窗口的行为测试：标题来源与"未保存改动"判定。

为什么需要：教师在一天里会反复新建/打开工程。旧实现只要导入过题库就
每次弹"未保存的修改会丢失"，久而久之会养成无脑点"是"的习惯，
真正需要拦截时反而会误点。
"""

from __future__ import annotations

import pytest

pytest.importorskip("PySide6")

from PySide6.QtWidgets import QMessageBox  # noqa: E402

from app.branding import branding  # noqa: E402
from app.ui.main_window import MainWindow  # noqa: E402


@pytest.fixture(autouse=True)
def isolate_settings(tmp_path, monkeypatch):
    """隔离本机设置：否则开发机上的 last_project 会让 MainWindow 自动打开真实工程。"""
    monkeypatch.setenv("LOCALAPPDATA", str(tmp_path / "localappdata"))


@pytest.fixture(scope="module")
def qapp():
    from PySide6.QtWidgets import QApplication

    app = QApplication.instance() or QApplication([])
    yield app


def test_window_title_uses_branding(qapp) -> None:
    win = MainWindow()
    try:
        # 标题 = 品牌名 + 管理端，后面可能跟一段状态提示（如"⚠ 配置有问题"）
        assert win.windowTitle().startswith(f"{branding().product} · 管理端")
    finally:
        win.deleteLater()


def test_clean_project_does_not_prompt(qapp) -> None:
    win = MainWindow()
    try:
        # 刚启动：没有改动，直接放行，不弹任何框
        assert win._dirty is False
        assert win._confirm_discard() is True
    finally:
        win.deleteLater()


def test_change_marks_dirty_and_save_clears_it(qapp, tmp_path, monkeypatch) -> None:
    win = MainWindow()
    try:
        win._on_changed()
        assert win._dirty is True

        from PySide6.QtWidgets import QFileDialog

        win.build_tab.chk_encrypt.setChecked(False)
        target = tmp_path / "p.json"
        monkeypatch.setattr(QFileDialog, "getSaveFileName", lambda *a, **k: (str(target), ""))
        win.save_project()
        assert target.is_file(), "应写出工程文件"
        assert win._dirty is False, "保存后应清除脏标记"
    finally:
        win.deleteLater()


def test_dirty_project_prompts_and_can_cancel(qapp, monkeypatch) -> None:
    asked: list[bool] = []

    def fake_question(*args, **kwargs):  # noqa: ANN002, ANN003
        asked.append(True)
        return QMessageBox.StandardButton.No

    monkeypatch.setattr(QMessageBox, "question", fake_question)

    win = MainWindow()
    try:
        win._on_changed()
        assert win._confirm_discard() is False, "选 No 时应阻止丢弃"
        assert asked, "有未保存改动时必须询问"
    finally:
        win.deleteLater()


def test_save_project_encrypted_when_checkbox(qapp, tmp_path, monkeypatch) -> None:
    from PySide6.QtWidgets import QFileDialog

    from app.core import secure
    from app.core.state import ProjectState

    win = MainWindow()
    try:
        win.build_tab.chk_encrypt.setChecked(True)
        monkeypatch.setattr(win, "_ask_passphrase", lambda *a, **k: "pw-123")
        target = tmp_path / "enc.eesproj"
        monkeypatch.setattr(QFileDialog, "getSaveFileName", lambda *a, **k: (str(target), ""))
        win.save_project()
        assert target.read_bytes().startswith(secure.MAGIC_PROJECT)
        assert ProjectState.load(target, "pw-123").students == []
    finally:
        win.deleteLater()


def test_encrypted_save_forces_eesproj_suffix(qapp, tmp_path, monkeypatch) -> None:
    from PySide6.QtWidgets import QFileDialog

    win = MainWindow()
    try:
        win.build_tab.chk_encrypt.setChecked(True)
        monkeypatch.setattr(win, "_ask_passphrase", lambda *a, **k: "long-enough-pw")
        target = tmp_path / "p.json"
        monkeypatch.setattr(QFileDialog, "getSaveFileName", lambda *a, **k: (str(target), "JSON"))
        win.save_project()
        assert (tmp_path / "p.eesproj").is_file()
        assert not target.exists()
    finally:
        win.deleteLater()


def test_on_built_respects_encrypt_checkbox(qapp, tmp_path, monkeypatch) -> None:
    from PySide6.QtWidgets import QMessageBox

    from app.core.state import ProjectState
    from app.ui.app_settings import PROJECT_DIR, settings

    monkeypatch.setattr(QMessageBox, "exec", lambda self: 0, raising=False)
    monkeypatch.setattr(QMessageBox, "warning", staticmethod(lambda *a, **k: None))
    win = MainWindow()
    try:
        settings().setValue(PROJECT_DIR, str(tmp_path))
        win.build_tab.chk_encrypt.setChecked(False)
        out = tmp_path / "out" / "包.exe"
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_bytes(b"x")
        win._on_built(str(out))
        assert win.project_path is not None
        # 明文工程：不带口令也能读
        assert ProjectState.load(win.project_path).students == []
    finally:
        win.deleteLater()
