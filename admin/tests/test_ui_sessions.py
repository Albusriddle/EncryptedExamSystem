"""场次 Tab 的交互回归。

用户真实反馈过两个症状：
1. 点「新建」没反应；
2. 点「保存此场次」提示"请先选中一个场次"。

根因分别是：模型禁止空科目导致槽函数抛异常被吞、以及 refresh 里
blockSignals 设置选中项导致表单没被填充（时间停在 2000-01-01）。

没有 PySide6 时整个模块跳过；CI 已安装 PySide6 并以 offscreen 平台真实运行。
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


def _wire_like_main_window(tab: SessionsTab) -> None:
    """复刻 MainWindow._on_changed 的接线。

    关键：真实程序里 `changed` 连到 `refresh_choices()`，
    而 refresh_choices 会**销毁重建**所有勾选框——这正是丢勾的触发条件。
    不接这条线，单独构造的 Tab 永远复现不出用户看到的问题。
    """
    tab.changed.connect(tab.refresh_choices)


def test_checks_survive_save(qapp, state_with_banks, mute_dialogs) -> None:
    """点「保存此场次」后，科目勾选不能消失。

    用户反馈：勾上课程保存后，勾全没了。根因是 save_session 末尾的
    `changed.emit()` 触发 refresh_choices()，把所有勾选框**销毁重建**，
    而 refresh() 里 blockSignals 不会重新加载表单，于是新控件全是未勾选。

    已有的 test_new_session_then_save_produces_valid_session 只断言了
    模型里的 s.exams ——数据是对的，所以它一直是绿的，界面却是错的。
    """
    tab = SessionsTab(state_with_banks)
    _wire_like_main_window(tab)
    tab.new_session()

    cb, pick, exam_id = tab.checks[0]
    cb.setChecked(True)
    pick.setValue(2)
    tab.save_session()

    assert len(state_with_banks.sessions[0].exams) == 1, "模型应有科目"
    same = next(c for c, _, eid in tab.checks if eid == exam_id)
    assert same.isChecked(), "保存后科目勾选被清空（界面与数据不一致）"
    same_pick = next(p for _, p, eid in tab.checks if eid == exam_id)
    assert int(same_pick.value()) == 2, "保存后抽题数丢失"


def test_checks_restored_when_reselecting_session(qapp, state_with_banks, mute_dialogs) -> None:
    """在左侧重新选中该场次，勾选必须能恢复。"""
    tab = SessionsTab(state_with_banks)
    _wire_like_main_window(tab)
    tab.new_session()
    cb, pick, exam_id = tab.checks[0]
    cb.setChecked(True)
    tab.save_session()

    # 模拟用户重新点列表里的该场次
    tab._on_select(tab.list.item(0), None)

    same = next(c for c, _, eid in tab.checks if eid == exam_id)
    assert same.isChecked(), "重新选中场次后勾选未恢复"


def test_unchecked_subject_stays_unchecked_after_save(qapp, state_with_banks, mute_dialogs) -> None:
    """只勾一门时，另一门保存后仍必须是未勾选（不能全部变成勾上）。"""
    tab = SessionsTab(state_with_banks)
    _wire_like_main_window(tab)
    tab.new_session()
    checked_id = tab.checks[0][2]
    tab.checks[0][0].setChecked(True)
    tab.save_session()

    for cb, _, eid in tab.checks:
        if eid != checked_id:
            assert not cb.isChecked(), f"{eid} 不应被勾上"


def test_build_log_shows_master_key(qapp, state_with_banks, tmp_path, monkeypatch) -> None:
    """打包页必须把主密钥显示在日志里，方便教师复制保存。

    这是刻意违反"master 不写日志"惯例的例外——用户明确要求。
    同时必须出现醒目警告，避免教师把日志截图发出去还不知道后果。
    """
    from app.ui import tab_build

    # 不让真的去起子进程：只验证日志内容
    monkeypatch.setattr(tab_build.BuildWorker, "start", lambda self: None)
    monkeypatch.setattr(tab_build.BuildWorker, "isRunning", lambda self: False)

    stub = tmp_path / "stub.exe"
    stub.write_bytes(b"fake")

    tab = tab_build.BuildTab(state_with_banks)
    tab.edit_stub.setText(str(stub))
    tab.edit_out.setText(str(tmp_path / "out.exe"))
    master = "ab" * 32  # 64 hex
    tab.edit_master.setText(master)

    # 先造一个合法场次（否则 start_build 会因配置问题提前返回）
    from app.core.models import Session, SessionExam

    state_with_banks.sessions = [
        Session(
            session_id=1,
            start="2026-10-23T11:00:00+08:00",
            end="2026-10-23T23:00:00+08:00",
            students=[202108010217],
            exams=[SessionExam(file="demo-math.json", pick=1)],
        )
    ]

    tab.start_build()

    log = tab.log_view.toPlainText()
    assert master in log, "主密钥没有显示在打包日志里"
    assert "请立即保存" in log
    assert "不要发给学生" in log, "缺少安全警告"
