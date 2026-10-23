"""导入确认对话框：导入前可以改题库 ID 与显示名称。

## 为什么需要

外部题库的文件名往往就是 `choice` / `fill` 这种通用词，直接当题库 ID 会
在工程里出现一堆看不出是什么科目的名字；而且两个不同目录的同名文件
还会互相撞名。与其让教师在文件管理器里改名，不如在导入这一步就能改。

## 校验

- 题库 ID 必须能安全用作文件名（复用 `validate_exam_id`，挡路径穿越）
- 不能为空、不能与本批其它题库重名
- 显示名称不能为空
"""

from __future__ import annotations

from PySide6.QtCore import Qt
from PySide6.QtWidgets import (
    QDialog,
    QDialogButtonBox,
    QHeaderView,
    QLabel,
    QMessageBox,
    QTableWidget,
    QTableWidgetItem,
    QVBoxLayout,
)

from app.core.bank_import import ImportedBank
from app.core.models import validate_exam_id


class ImportConfirmDialog(QDialog):
    """列出本批要导入的题库，允许改 ID 与名称。"""

    def __init__(self, items: list[ImportedBank], parent=None) -> None:
        super().__init__(parent)
        self.setWindowTitle("确认导入")
        self.resize(760, 380)
        self._items = items

        lay = QVBoxLayout(self)
        lay.addWidget(
            QLabel(
                "将导入以下题库。可以直接修改「题库 ID」（用于文件名）和「显示名称」。\n"
                "题库 ID 只能是文件名安全的字符，且不能重复。"
            )
        )

        self.table = QTableWidget(len(items), 4, self)
        self.table.setHorizontalHeaderLabels(["源文件", "题库 ID", "显示名称", "题数"])
        for row, item in enumerate(items):
            self.table.setItem(row, 0, self._readonly(item.source.name))
            self.table.setItem(row, 1, QTableWidgetItem(item.bank.exam_id))
            self.table.setItem(row, 2, QTableWidgetItem(item.bank.title))
            self.table.setItem(row, 3, self._readonly(str(len(item.bank.questions))))

        header = self.table.horizontalHeader()
        header.setSectionResizeMode(0, QHeaderView.ResizeMode.ResizeToContents)
        header.setSectionResizeMode(1, QHeaderView.ResizeMode.ResizeToContents)
        header.setSectionResizeMode(2, QHeaderView.ResizeMode.Stretch)
        header.setSectionResizeMode(3, QHeaderView.ResizeMode.ResizeToContents)
        lay.addWidget(self.table, 1)

        buttons = QDialogButtonBox(
            QDialogButtonBox.StandardButton.Ok | QDialogButtonBox.StandardButton.Cancel
        )
        buttons.button(QDialogButtonBox.StandardButton.Ok).setText("导入")
        buttons.button(QDialogButtonBox.StandardButton.Cancel).setText("取消")
        buttons.accepted.connect(self.accept)
        buttons.rejected.connect(self.reject)
        lay.addWidget(buttons)

    @staticmethod
    def _readonly(text: str) -> QTableWidgetItem:
        item = QTableWidgetItem(text)
        item.setFlags(Qt.ItemFlag.ItemIsEnabled)
        return item

    def accept(self) -> None:
        """校验通过才真正写入题库对象。"""
        ids: list[str] = []
        for row, item in enumerate(self._items):
            raw_id = self.table.item(row, 1).text().strip()
            title = self.table.item(row, 2).text().strip()
            try:
                exam_id = validate_exam_id(raw_id)
            except ValueError as e:
                QMessageBox.warning(self, "题库 ID 不合法", f"{item.source.name}：{e}")
                return
            if exam_id in ids:
                QMessageBox.warning(
                    self, "题库 ID 重复", f"「{exam_id}」出现了两次，请改成不同的名字。"
                )
                return
            if not title:
                QMessageBox.warning(self, "显示名称为空", f"{item.source.name}：显示名称不能为空。")
                return
            ids.append(exam_id)

        for row, item in enumerate(self._items):
            item.bank.exam_id = self.table.item(row, 1).text().strip()
            item.bank.title = self.table.item(row, 2).text().strip()
        super().accept()
