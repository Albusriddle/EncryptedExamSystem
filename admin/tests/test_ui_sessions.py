"""场次 Tab 的交互回归。

用户真实反馈过两个症状：
1. 点「新建」没反应；
2. 点「保存此场次」提示"请先选中一个场次"。

根因分别是：模型禁止空科目导致槽函数抛异常被吞、以及 refresh 里
blockSignals 设置选中项导致表单没被填充（时间停在 2000-01-01）。

没有 PySide6 时整个模块跳过（CI 的 python job 不装 GUI 依赖）。
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

pytest.importorskip("PySide6")

from PySide6.QtWidgets import QApplication, QMessageBox  # noqa: E402

from app.core.models import ExamBank  # noqa: E402
from app.core.state import ProjectState  # noqa: E402
from app.ui.tab_sessions import SessionsTab  # noqa: E402


@pytest.fixture(scope="module")
def qapp():
    app = QApplication.instance() or QApplication(sys.argv)
    yield app


@pytest.fixture(autouse=True)
def mute_dialogs(monkeypatch: pytest.MonkeyPatch):
    """弹窗会阻塞测试，全部静音并记录。"""
    calls: list[tuple] = []

    def rec(kind):
        def f(parent, title, text, *a, **k):  # noqa: ANN001
            calls.append((kind, title, str(text)))

        return staticmethod(f)

    for name in ("information", "warning", "critical"):
        monkeypatch.setattr(QMessageBox, name, rec(name))
    return calls


@pytest.fixture
def state_with_banks() -> ProjectState:
    root = Path(__file__).resolve().parents[2]
    st = ProjectState()
    for n in ("math", "english"):
        p = root / "banks" / "demo" / f"{n}.json"
        st.add_bank(ExamBank.model_validate_json(p.read_text(encoding="utf-8")), str(p))
    st.students = [202108010217]
    return st


def test_new_session_creates_row_and_fills_form(qapp, state_with_banks) -> None:
    """点「新建」必须真的产生场次，并把表单填成合法时间。"""
    tab = SessionsTab(state_with_banks)
    assert tab.list.count() == 0

    tab.new_session()

    assert len(state_with_banks.sessions) == 1
    assert tab.list.count() == 1
    # 表单时间不能停在 Qt 默认的 2000-01-01
    assert tab.dt_start.dateTime().toString("yyyy") != "2000"
    assert tab.dt_start.dateTime() < tab.dt_end.dateTime()


def test_new_session_then_save_produces_valid_session(qapp, state_with_banks) -> None:
    tab = SessionsTab(state_with_banks)
    tab.new_session()

    # 勾选一门科目
    cb, pick, _exam_id = tab.checks[0]
    cb.setChecked(True)
    pick.setValue(2)
    tab.save_session()

    s = state_with_banks.sessions[0]
    assert len(s.exams) == 1
    assert s.exams[0].pick == 2
    assert state_with_banks.validate() == []


def test_save_without_selection_creates_instead_of_refusing(
    qapp, state_with_banks, mute_dialogs
) -> None:
    """未选中场次时保存，应按"新建"处理，而不是弹"请先选中一个场次"。"""
    # 真实场景：刚打开程序，一个场次都没有，教师直接填表单点保存
    tab = SessionsTab(state_with_banks)
    assert tab._current is None
    tab.checks[0][0].setChecked(True)
    tab.save_session()

    titles = [t for _, t, _ in mute_dialogs]
    assert "提示" not in titles, f"不该拒绝保存：{titles}"
    assert len(state_with_banks.sessions) == 1
    assert len(state_with_banks.sessions[0].exams) == 1


def test_subject_checkboxes_exist_after_construction(qapp, state_with_banks) -> None:
    """打开已有工程（题库已存在）时，科目勾选框必须已经建好。"""
    tab = SessionsTab(state_with_banks)
    assert len(tab.checks) == 2
