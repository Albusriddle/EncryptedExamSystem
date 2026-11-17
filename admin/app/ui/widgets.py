"""小型 UI 工具。"""

from __future__ import annotations

from PySide6.QtWidgets import (
    QCheckBox,
    QComboBox,
    QDateTimeEdit,
    QPushButton,
    QSizePolicy,
    QSpinBox,
    QTabBar,
)


def fit_button_text(button: QPushButton) -> None:
    """按实际字体宽度设置最小宽度，避免中文按钮被压成 ``...``。

    ``sizeHint()`` 在样式表/DPI 变化后可能偏小；这里直接量文字宽度，
    再留出左右内边距。只设 minimumWidth，不设 fixedWidth，窗口仍可调整。
    """
    button.ensurePolished()
    text_width = button.fontMetrics().horizontalAdvance(button.text())
    button.setMinimumWidth(text_width + 32)
    button.setSizePolicy(QSizePolicy.Policy.Minimum, QSizePolicy.Policy.Fixed)


class _NoWheelMixin:
    """滚轮不改值，也不吞事件。

    直接 ``event.ignore()`` 让事件继续冒泡到父级 ``QScrollArea``：
    实测这是唯一同时满足「值不变」和「页面还能滚」的写法。
    ``setFocusPolicy`` 在 Qt 6.12 无效（无焦点仍然改值）；
    用 ``eventFilter`` 吞掉事件会连页面滚动一起废。
    """

    def wheelEvent(self, event) -> None:  # noqa: N802 (Qt 命名)
        event.ignore()


class NoWheelComboBox(_NoWheelMixin, QComboBox):
    """下拉框：滚轮不切换选中项。"""


class NoWheelSpinBox(_NoWheelMixin, QSpinBox):
    """数字框：滚轮不改值。"""


class NoWheelDateTimeEdit(_NoWheelMixin, QDateTimeEdit):
    """时间框：滚轮不改日期/时间。"""


class NoWheelTabBar(_NoWheelMixin, QTabBar):
    """页签栏：滚轮不切页。"""


def compact_checkbox(cb: QCheckBox) -> QCheckBox:
    """把勾选框宽度收窄到「方框 + 文字」，限制误点范围。

    为什么必须限宽：Windows 11 样式下 ``SE_CheckBoxClickRect`` 等于**整个控件矩形**
    （实测 872px 宽的行点空白也会切换），只缩短文案并不会缩小可点面积。
    固定宽度后，可点面积 = 文字 + 方框，和 Fusion/vista 行为一致。
    """
    cb.ensurePolished()
    cb.setSizePolicy(QSizePolicy.Policy.Fixed, QSizePolicy.Policy.Fixed)
    cb.setMaximumWidth(cb.sizeHint().width())
    return cb
