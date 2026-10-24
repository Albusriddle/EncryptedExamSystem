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


def test_copy_master_button_puts_key_on_clipboard(qapp, state_with_banks, tmp_path) -> None:
    """主密钥只能靠「复制主密钥」按钮导出到剪贴板，并给出确认提示。"""
    from PySide6.QtGui import QGuiApplication

    from app.ui import tab_build

    tab = tab_build.BuildTab(state_with_banks)
    master = "ab" * 32
    tab.edit_master.setText(master)

    tab.copy_master()

    assert QGuiApplication.clipboard().text() == master
    assert "已复制到剪贴板" in tab.copy_hint.text()
    assert "妥善保管" in tab.copy_hint.text()


def test_copy_master_without_key_warns(qapp, state_with_banks) -> None:
    from app.ui import tab_build

    tab = tab_build.BuildTab(state_with_banks)
    tab.edit_master.setText("")
    tab.copy_master()
    assert "还没有主密钥" in tab.copy_hint.text()


def test_master_never_written_to_build_log(qapp, state_with_banks, tmp_path, monkeypatch) -> None:
    """安全红线：master 绝不能出现在日志区（日志会被截图/留在共享电脑上）。"""
    from app.core.models import Session, SessionExam
    from app.ui import tab_build

    monkeypatch.setattr(tab_build.BuildWorker, "start", lambda self: None)
    monkeypatch.setattr(tab_build.BuildWorker, "isRunning", lambda self: False)

    stub = tmp_path / "stub.exe"
    stub.write_bytes(b"fake")

    tab = tab_build.BuildTab(state_with_banks)
    tab.edit_stub.setText(str(stub))
    tab.edit_out.setText(str(tmp_path / "out.exe"))
    master = "cd" * 32
    tab.edit_master.setText(master)
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
    try:
        assert master not in tab.log_view.toPlainText(), "主密钥泄漏到日志"
        # 打包后输入框仍保留密钥，教师才能点「复制主密钥」
        assert tab.edit_master.text() == master
    finally:
        tab.worker = None
        tab._cleanup_tmp()
        tab.deleteLater()


def test_cancel_reports_cancelled_not_failure(
    qapp, state_with_banks, tmp_path, monkeypatch
) -> None:
    """取消打包不该弹「打包失败」——那会让人以为程序坏了。"""
    from PySide6.QtWidgets import QMessageBox

    from app.ui import tab_build

    shown: list[tuple[str, str]] = []
    monkeypatch.setattr(
        QMessageBox,
        "information",
        lambda *a, **k: shown.append(("info", a[1] if len(a) > 1 else "")),
    )
    monkeypatch.setattr(
        QMessageBox, "critical", lambda *a, **k: shown.append(("crit", a[1] if len(a) > 1 else ""))
    )

    tab = tab_build.BuildTab(state_with_banks)
    tab.edit_out.setText(str(tmp_path / "out.exe"))
    tab._cancelled = True
    tab._on_done(False, "打包程序退出码 1")

    assert shown and shown[-1][0] == "info", f"应弹信息框而非错误框，实际 {shown}"
    assert "已取消" in shown[-1][1]
    assert "已取消" in tab.log_view.toPlainText()


def test_cancel_removes_partial_tmp(qapp, state_with_banks, tmp_path) -> None:
    from app.ui import tab_build

    out = tmp_path / "out.exe"
    leftover = tmp_path / "out.exe.tmp"
    leftover.write_bytes(b"partial")

    tab = tab_build.BuildTab(state_with_banks)
    tab.edit_out.setText(str(out))
    tab._cancelled = True
    tab._on_done(False, "打包程序退出码 1")

    assert not leftover.exists(), "取消后应清理 .tmp 半成品"


def test_copy_verifies_clipboard_readback(qapp, state_with_banks, monkeypatch) -> None:
    """剪贴板被占用、setText 静默失败时，不能谎报"已复制"。"""
    from app.ui import tab_build

    class _DeadClipboard:
        def setText(self, _text: str) -> None:  # noqa: N802 - 模仿 Qt 命名
            pass

        def text(self) -> str:
            return ""

    class _FakeApp:
        @staticmethod
        def clipboard():  # noqa: ANN205
            return _DeadClipboard()

    monkeypatch.setattr(tab_build, "QGuiApplication", _FakeApp)

    tab = tab_build.BuildTab(state_with_banks)
    tab.edit_master.setText("ab" * 32)
    tab.copy_master()

    assert "复制失败" in tab.copy_hint.text()


def test_master_auto_cleared_after_timeout(qapp, state_with_banks) -> None:
    """复制 5 分钟后自动清空，避免密钥长期留在界面上。"""
    from app.ui import tab_build

    tab = tab_build.BuildTab(state_with_banks)
    key = "ef" * 32
    tab.edit_master.setText(key)
    tab.copy_master()
    assert tab.edit_master.text() == key

    tab._clear_master_if_unchanged(key)

    assert tab.edit_master.text() == ""
    assert "自动清空" in tab.copy_hint.text()


def test_auto_clear_keeps_user_edited_value(qapp, state_with_banks) -> None:
    """教师已经改成别的密钥时，定时器不能把它抹掉。"""
    from app.ui import tab_build

    tab = tab_build.BuildTab(state_with_banks)
    tab.edit_master.setText("11" * 32)
    tab._clear_master_if_unchanged("22" * 32)
    assert tab.edit_master.text() == "11" * 32


def test_auto_clear_keeps_key_when_clipboard_changed(qapp, state_with_banks) -> None:
    """剪贴板被别的内容覆盖时，输入框是唯一副本——绝不能自动清空。"""
    from PySide6.QtGui import QGuiApplication

    from app.ui import tab_build

    tab = tab_build.BuildTab(state_with_banks)
    key = "cd" * 32
    tab.edit_master.setText(key)
    QGuiApplication.clipboard().setText("别的内容")  # 模拟被覆盖

    tab._clear_master_if_unchanged(key)

    assert tab.edit_master.text() == key, "剪贴板已变化时不应清空唯一副本"
    assert "保留" in tab.copy_hint.text()


def test_manual_clear_button(qapp, state_with_banks) -> None:
    from app.ui import tab_build

    tab = tab_build.BuildTab(state_with_banks)
    tab.edit_master.setText("12" * 32)
    tab.clear_master()
    assert tab.edit_master.text() == ""
    assert "已清空" in tab.copy_hint.text()


# ---------------------------------------------------------------- 抽题数输入框


def test_pick_width_scales_with_digits() -> None:
    """宽度必须随位数增长，否则 500 会被画成 50。"""
    from app.ui.tab_sessions import pick_width

    assert pick_width(5) >= 75  # 至少两位数宽度
    assert pick_width(50) >= 75
    assert pick_width(500) >= 90  # 三位数不能比两位数还窄
    assert pick_width(5000) >= 100
    # 单调不减：位数越多越宽
    assert pick_width(5000) > pick_width(500) > pick_width(50)


def test_pick_spinbox_is_wide_enough(qapp, state_with_banks) -> None:
    """500 题的题库：控件必须是 QSpinBox，且宽度装得下 3 位数。"""
    from PySide6.QtWidgets import QSpinBox

    from app.core.models import ExamBank, Question
    from app.ui.tab_sessions import SessionsTab

    bank = ExamBank(
        exam_id="big",
        title="big",
        pick=500,
        questions=[
            Question(id=f"q{i}", type="single", stem="s", options=["a"]) for i in range(500)
        ],
    )
    state_with_banks.banks["big"] = bank
    editor = SessionsTab(state_with_banks)
    try:
        editor.refresh_choices()
        cbs = [c for c, _p, eid in editor.checks if eid == "big"]
        picks = [p for _c, p, eid in editor.checks if eid == "big"]
        assert cbs and picks
        pick = picks[0]
        assert isinstance(pick, QSpinBox)
        assert pick.value() == 500, "值本身不能被截断"
        assert pick.minimumWidth() >= 90
    finally:
        editor.deleteLater()


def test_bank_label_avoids_duplicate_name(qapp, state_with_banks) -> None:
    """title == exam_id 时不要在括号里重复显示同一个名字。"""
    from PySide6.QtWidgets import QCheckBox

    from app.core.models import ExamBank, Question
    from app.ui.tab_sessions import SessionsTab

    # 外部题库导入时 title 默认等于文件名，这里复刻那种情况
    state_with_banks.banks["same"] = ExamBank(
        exam_id="same",
        title="same",
        pick=1,
        questions=[Question(id="q", type="single", stem="s", options=["a"])],
    )
    editor = SessionsTab(state_with_banks)
    try:
        editor.refresh_choices()
        labels = [cb.text() for cb in editor.findChildren(QCheckBox)]
        assert "same（题量 1）" in labels, labels
        assert not [x for x in labels if x.count("same") > 1], f"名称重复显示：{labels}"
    finally:
        editor.deleteLater()


# ---------------------------------------------------------------- 班级（第十四轮之后）


def test_by_class_selection_fills_roster(qapp, state_with_banks, monkeypatch) -> None:
    """选班级后：学号并入自定义名单，并切回 custom 模式（场次存快照）。"""
    from app.core.models import StudentClass

    state_with_banks.students = [1, 2, 3, 4]
    state_with_banks.classes = [
        StudentClass(name="一班", code="C1", students=[1, 2]),
        StudentClass(name="二班", code="C2", students=[3]),
    ]
    tab = SessionsTab(state_with_banks)
    try:
        monkeypatch.setattr(tab, "_pick_classes", lambda: [1, 2, 3])
        tab.combo_roster.setCurrentIndex(tab.combo_roster.findData("by_class"))
        assert tab.combo_roster.currentData() == "custom", "选完必须切回自定义（存快照）"
        assert tab.edit_roster.text().split() == ["1", "2", "3"]
        assert "来自 2 个班" in tab.lbl_roster_info.text()
    finally:
        tab.deleteLater()


def test_roster_info_warns_when_no_class_matches(qapp, state_with_banks) -> None:
    from app.core.models import StudentClass

    state_with_banks.students = [1, 2]
    state_with_banks.classes = [StudentClass(name="一班", students=[1])]
    tab = SessionsTab(state_with_banks)
    try:
        tab.edit_roster.setText("2 9")
        assert "未匹配到班级" in tab.lbl_roster_info.text()
    finally:
        tab.deleteLater()
