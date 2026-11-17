"""交互回归：滚轮不改值、勾选框可点范围受限、危险切换要确认。"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest
from PySide6.QtCore import QDate, QDateTime, QPoint, QPointF, Qt, QTime
from PySide6.QtGui import QWheelEvent
from PySide6.QtWidgets import QApplication, QCheckBox, QTabWidget, QWidget

ROOT = Path(__file__).resolve().parents[2]


@pytest.fixture(scope="module")
def qapp():
    app = QApplication.instance() or QApplication(sys.argv)
    yield app


@pytest.fixture
def state_with_banks():
    from app.core.models import ExamBank
    from app.core.state import ProjectState

    st = ProjectState()
    for name in ("math", "english"):
        path = ROOT / "banks" / "demo" / f"{name}.json"
        st.add_bank(ExamBank.model_validate_json(path.read_text(encoding="utf-8")), str(path))
    st.students = [202108010217]
    return st


def _wheel(widget: QWidget, delta: int = -120) -> QWheelEvent:
    pos = QPointF(widget.rect().center())
    event = QWheelEvent(
        pos,
        widget.mapToGlobal(QPoint(int(pos.x()), int(pos.y()))),
        QPoint(0, 0),
        QPoint(0, delta),
        Qt.MouseButton.NoButton,
        Qt.KeyboardModifier.NoModifier,
        Qt.ScrollPhase.NoScrollPhase,
        False,
    )
    QApplication.sendEvent(widget, event)
    return event


def test_no_wheel_spin_keeps_value(qapp) -> None:
    from app.ui.widgets import NoWheelSpinBox

    spin = NoWheelSpinBox()
    spin.setRange(0, 100)
    spin.setValue(50)
    event = _wheel(spin)
    assert spin.value() == 50, "滚轮不应改变数字框的值"
    assert event.isAccepted() is False, "必须 ignore，事件才能冒泡给滚动区"


def test_no_wheel_combo_keeps_index(qapp) -> None:
    from app.ui.widgets import NoWheelComboBox

    combo = NoWheelComboBox()
    combo.addItems(["a", "b", "c"])
    combo.setCurrentIndex(1)
    _wheel(combo)
    assert combo.currentIndex() == 1, "滚轮不应切换下拉项"


def test_no_wheel_datetime_keeps_value(qapp) -> None:
    from app.ui.widgets import NoWheelDateTimeEdit

    edit = NoWheelDateTimeEdit()
    edit.setDateTime(QDateTime(QDate(2026, 11, 18), QTime(9, 0)))
    before = edit.dateTime()
    _wheel(edit)
    assert edit.dateTime() == before, "滚轮不应改日期/时间（默认会整年跳）"


def test_no_wheel_tabbar_keeps_page(qapp) -> None:
    from app.ui.widgets import NoWheelTabBar

    tabs = QTabWidget()
    tabs.setTabBar(NoWheelTabBar())
    tabs.addTab(QWidget(), "A")
    tabs.addTab(QWidget(), "B")
    tabs.addTab(QWidget(), "C")
    tabs.setCurrentIndex(1)
    _wheel(tabs.tabBar())
    assert tabs.currentIndex() == 1, "滚轮停在页签上不应切页"


def test_compact_checkbox_limits_hit_area(qapp) -> None:
    from app.ui.widgets import compact_checkbox

    plain = QCheckBox("按大题模式")
    box = compact_checkbox(QCheckBox("按大题模式"))
    assert box.maximumWidth() == box.sizeHint().width()
    assert box.maximumWidth() <= plain.sizeHint().width() + 4


def test_encrypt_toggle_needs_confirmation(qapp, state_with_banks, monkeypatch, tmp_path) -> None:
    from PySide6.QtWidgets import QMessageBox

    from app.ui import app_settings
    from app.ui.tab_build import BuildTab

    class FakeSettings:
        def __init__(self) -> None:
            self.data = {app_settings.PROJECT_ENCRYPT: True}

        def value(self, key, default=None, type=None):  # noqa: A002
            return self.data.get(key, default)

        def setValue(self, key, value) -> None:  # noqa: N802
            self.data[key] = value

    fake = FakeSettings()
    monkeypatch.setattr(app_settings, "settings", lambda: fake)
    monkeypatch.setattr(
        QMessageBox, "question", staticmethod(lambda *a, **k: QMessageBox.StandardButton.No)
    )

    tab = BuildTab(state_with_banks)
    try:
        assert tab.chk_encrypt.isChecked() is True
        tab.chk_encrypt.click()  # 用户点击取消勾选
        assert tab.chk_encrypt.isChecked() is True, "确认框选“否”后必须保持勾选"
        assert fake.data[app_settings.PROJECT_ENCRYPT] is True, "不能把关闭状态的加密写进设置"
    finally:
        tab.deleteLater()


def test_encrypt_toggle_applies_after_confirmation(qapp, state_with_banks, monkeypatch) -> None:
    from PySide6.QtWidgets import QMessageBox

    from app.ui import app_settings
    from app.ui.tab_build import BuildTab

    class FakeSettings:
        def __init__(self) -> None:
            self.data = {app_settings.PROJECT_ENCRYPT: True}

        def value(self, key, default=None, type=None):  # noqa: A002
            return self.data.get(key, default)

        def setValue(self, key, value) -> None:  # noqa: N802
            self.data[key] = value

    fake = FakeSettings()
    monkeypatch.setattr(app_settings, "settings", lambda: fake)
    monkeypatch.setattr(
        QMessageBox, "question", staticmethod(lambda *a, **k: QMessageBox.StandardButton.Yes)
    )

    tab = BuildTab(state_with_banks)
    try:
        tab.chk_encrypt.click()
        assert tab.chk_encrypt.isChecked() is False
        assert fake.data[app_settings.PROJECT_ENCRYPT] is False
    finally:
        tab.deleteLater()


def test_sections_toggle_needs_confirmation(qapp, state_with_banks, monkeypatch) -> None:
    from PySide6.QtWidgets import QMessageBox

    from app.ui.tab_sessions import SessionsTab

    monkeypatch.setattr(
        QMessageBox, "question", staticmethod(lambda *a, **k: QMessageBox.StandardButton.No)
    )
    tab = SessionsTab(state_with_banks)
    try:
        tab.chk_sections.setChecked(True)  # 程序化打开：不弹确认
        assert tab.section_rows, "打开后应自动建一行"
        tab.chk_sections.click()  # 用户点击关闭
        assert tab.chk_sections.isChecked() is True, "确认框选“否”后必须保持打开"
    finally:
        tab.deleteLater()
