"""新工程应显示仓库示例题库，但不能把默认题库混入已保存工程。"""

from __future__ import annotations

from pathlib import Path

import pytest

pytest.importorskip("PySide6")

from PySide6.QtWidgets import QApplication  # noqa: E402

from app.core.bank_import import load_bank_from_path  # noqa: E402
from app.core.state import ProjectState  # noqa: E402
from app.ui.main_window import MainWindow  # noqa: E402

DEMO = Path(__file__).resolve().parents[2] / "banks" / "demo"


@pytest.fixture(scope="module")
def qapp():
    return QApplication.instance() or QApplication([])


@pytest.fixture(autouse=True)
def isolated_settings(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("LOCALAPPDATA", str(tmp_path / "settings"))


def _assert_demo_banks_visible(win: MainWindow) -> None:
    expected = {
        bank.exam_id: bank
        for path in sorted(DEMO.glob("*.json"))
        if path.name != "manifest.example.json"
        for bank in [load_bank_from_path(path)]
    }
    table = win.banks_tab.table
    assert table.rowCount() == len(expected)
    for row in range(table.rowCount()):
        bank = expected[table.item(row, 0).text()]
        assert table.item(row, 1).text() == bank.title
        assert table.item(row, 2).text() == str(len(bank.questions))
        assert table.item(row, 3).text() == str(bank.pick)
    assert {row.exam_id for row in win.sessions_tab.checks} == set(expected)
    assert "示例题库" in win.statusBar().currentMessage()
    assert "演示" in win.statusBar().currentMessage()
    assert win.state.students == []
    assert win.state.sessions == []
    assert win.project_path is None
    assert not win._dirty


def test_startup_displays_repository_demo_banks(qapp, monkeypatch: pytest.MonkeyPatch) -> None:
    # 启动工作目录不应影响默认题库的定位。
    monkeypatch.chdir(Path.home())
    win = MainWindow()
    try:
        _assert_demo_banks_visible(win)
        assert win._confirm_discard()
    finally:
        win.deleteLater()


def test_new_project_restores_removed_demo_banks(qapp) -> None:
    win = MainWindow()
    try:
        win.state.banks.clear()
        win.state.bank_paths.clear()
        win.banks_tab.refresh()
        assert win.banks_tab.table.rowCount() == 0
        win.new_project()
        _assert_demo_banks_visible(win)
    finally:
        win.deleteLater()


def test_opened_project_does_not_gain_default_banks(qapp, tmp_path: Path) -> None:
    project = tmp_path / "empty-project.json"
    ProjectState(students=[7]).save(project)
    win = MainWindow()
    try:
        assert win.banks_tab.table.rowCount() > 0
        assert win._load_project(project)
        assert win.banks_tab.table.rowCount() == 0
        assert win.state.banks == {}
        assert win.state.students == [7]
        assert win.project_path == project
    finally:
        win.deleteLater()


@pytest.mark.parametrize("passphrase", [None, "demo-project-password"])
def test_saved_appimage_project_reopens_after_extraction_is_removed(
    qapp, tmp_path: Path, monkeypatch: pytest.MonkeyPatch, passphrase: str | None
) -> None:
    import shutil
    import sys

    resources = tmp_path / "first-extraction" / "_internal"
    shutil.copytree(DEMO, resources / "banks" / "demo")
    monkeypatch.setattr(sys, "_MEIPASS", str(resources), raising=False)
    win = MainWindow()
    project = tmp_path / "demo.eesproj"
    try:
        _assert_demo_banks_visible(win)
        win.state.save(project, passphrase=passphrase)
    finally:
        win.deleteLater()
    shutil.rmtree(resources.parent)

    # 下次启动会解压到另一个目录；工程里不能再引用已删除的第一次解压路径。
    next_resources = tmp_path / "second-extraction" / "_internal"
    shutil.copytree(DEMO, next_resources / "banks" / "demo")
    monkeypatch.setattr(sys, "_MEIPASS", str(next_resources), raising=False)
    reopened = MainWindow()
    monkeypatch.setattr(reopened, "_ask_passphrase", lambda _title: passphrase)
    try:
        assert reopened._load_project(project)
        expected = {
            bank.exam_id: bank
            for path in DEMO.glob("*.json")
            if path.name != "manifest.example.json"
            for bank in [load_bank_from_path(path)]
        }
        table = reopened.banks_tab.table
        assert table.rowCount() == len(expected)
        for row in range(table.rowCount()):
            bank = expected[table.item(row, 0).text()]
            assert table.item(row, 1).text() == bank.title
            assert table.item(row, 2).text() == str(len(bank.questions))
        assert {row.exam_id for row in reopened.sessions_tab.checks} == set(reopened.state.banks)
        assert all(Path(path).is_file() for path in reopened.state.bank_paths.values())
        assert "源文件读不到" not in reopened.statusBar().currentMessage()
    finally:
        reopened.deleteLater()


def test_unwritable_demo_data_directory_allows_manual_import(qapp, tmp_path, monkeypatch) -> None:
    import shutil
    import sys

    from app.core import default_banks

    resources = tmp_path / "resources"
    shutil.copytree(DEMO, resources / "banks" / "demo")
    monkeypatch.setattr(sys, "_MEIPASS", str(resources), raising=False)
    blocked = tmp_path / "not-a-directory"
    blocked.write_text("block writes", encoding="utf-8")
    monkeypatch.setattr(default_banks, "data_dir", lambda: blocked)
    win = MainWindow()
    try:
        assert win.banks_tab.table.rowCount() == 0
        assert win.banks_tab.btn_add.isEnabled()
        assert "用户数据目录" in win.statusBar().currentMessage()
        assert "检查目录权限或手动导入" in win.statusBar().currentMessage()
    finally:
        win.deleteLater()


def test_missing_demo_resources_allow_manual_import(qapp, tmp_path, monkeypatch) -> None:
    from app.core import default_banks

    monkeypatch.setattr(default_banks, "demo_banks_dir", lambda: tmp_path / "missing")
    win = MainWindow()
    try:
        assert win.banks_tab.table.rowCount() == 0
        assert win.banks_tab.btn_add.isEnabled()
        message = win.statusBar().currentMessage()
        assert "示例题库加载失败" in message
        assert "导入题库" in message
        assert "重新安装" in message
    finally:
        win.deleteLater()
