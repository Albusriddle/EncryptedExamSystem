"""小型 UI 工具。"""

from __future__ import annotations

from PySide6.QtWidgets import QPushButton, QSizePolicy


def fit_button_text(button: QPushButton) -> None:
    """按实际字体宽度设置最小宽度，避免中文按钮被压成 ``...``。

    ``sizeHint()`` 在样式表/DPI 变化后可能偏小；这里直接量文字宽度，
    再留出左右内边距。只设 minimumWidth，不设 fixedWidth，窗口仍可调整。
    """
    button.ensurePolished()
    text_width = button.fontMetrics().horizontalAdvance(button.text())
    button.setMinimumWidth(text_width + 32)
    button.setSizePolicy(QSizePolicy.Policy.Minimum, QSizePolicy.Policy.Fixed)
