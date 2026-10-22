"""题库 Tab：导入/移除 JSON 题库，查看题量与抽题数。"""

from __future__ import annotations

from pathlib import Path

from PySide6.QtCore import Qt, Signal
from PySide6.QtWidgets import (
    QFileDialog,
    QHBoxLayout,
    QHeaderView,
    QMessageBox,
    QPushButton,
    QTableWidget,
    QTableWidgetItem,
    QVBoxLayout,
    QWidget,
)

from app.core.models import ExamBank
from app.core.state import ProjectState


class BanksTab(QWidget):
    changed = Signal()

    def __init__(self, state: ProjectState) -> None:
        super().__init__()
        self.state = state
        self._build_ui()
        self.refresh()

    def _build_ui(self) -> None:
        lay = QVBoxLayout(self)

        bar = QHBoxLayout()
        self.btn_add = QPushButton("导入题库…")
        self.btn_add.clicked.connect(self.import_banks)
        self.btn_remove = QPushButton("移除选中")
        self.btn_remove.clicked.connect(self.remove_selected)
        bar.addWidget(self.btn_add)
        bar.addWidget(self.btn_remove)
        bar.addStretch(1)
        lay.addLayout(bar)

        self.table = QTableWidget(0, 4)
        self.table.setHorizontalHeaderLabels(["exam_id", "标题", "题量", "抽题数"])
        self.table.horizontalHeader().setSectionResizeMode(1, QHeaderView.ResizeMode.Stretch)
        self.table.setSelectionBehavior(QTableWidget.SelectionBehavior.SelectRows)
        self.table.setEditTriggers(QTableWidget.EditTrigger.NoEditTriggers)
        self.table.doubleClicked.connect(self.preview_selected)
        lay.addWidget(self.table)

    # ---------------------------------------------------------------- 动作

    def import_banks(self) -> None:
        paths, _ = QFileDialog.getOpenFileNames(self, "选择题库文件（可多选）", "", "JSON (*.json)")
        if not paths:
            return
        ok, errors = 0, []
        for p in paths:
            try:
                # 显式 UTF-8：中文题库最常见的坑就是被存成 GBK
                text = Path(p).read_text(encoding="utf-8")
                bank = ExamBank.model_validate_json(text)
            except UnicodeDecodeError:
                errors.append(
                    f"{Path(p).name}：不是 UTF-8 编码。\n"
                    "  请用支持 UTF-8 的编辑器另存（记事本选「UTF-8」；\n"
                    "  VS Code 右下角点击编码 → Save with Encoding → UTF-8）。"
                )
                continue
            except Exception as e:  # noqa: BLE001 - pydantic 的 ValidationError
                errors.append(f"{Path(p).name}：\n{e}")
                continue
            self.state.add_bank(bank, str(Path(p).resolve()))
            ok += 1

        self.refresh()
        if ok:
            self.changed.emit()

        if errors:
            QMessageBox.warning(
                self,
                f"导入完成：成功 {ok} 个，失败 {len(errors)} 个",
                "\n\n".join(errors),
            )
        elif ok:
            QMessageBox.information(self, "导入成功", f"成功导入 {ok} 个题库。")

    def remove_selected(self) -> None:
        rows = sorted({i.row() for i in self.table.selectedIndexes()}, reverse=True)
        if not rows:
            QMessageBox.information(self, "提示", "请先选中要移除的题库。")
            return
        names = [self.table.item(r, 0).text() for r in rows]
        ret = QMessageBox.question(
            self,
            "确认移除",
            "将移除以下题库，并从所有场次中摘除其引用：\n\n" + "\n".join(names),
        )
        if ret != QMessageBox.StandardButton.Yes:
            return
        for n in names:
            self.state.remove_bank(n)
        self.refresh()
        self.changed.emit()

    def preview_selected(self) -> None:
        rows = sorted({i.row() for i in self.table.selectedIndexes()})
        if not rows:
            return
        exam_id = self.table.item(rows[0], 0).text()
        bank = self.state.banks.get(exam_id)
        if bank is None:
            return
        lines = [f"{bank.title}（{exam_id}）共 {len(bank.questions)} 题", ""]
        for q in bank.questions[:10]:
            lines.append(f"[{q.qtype}] {q.stem}")
            for i, opt in enumerate(q.options):
                lines.append(f"    {chr(ord('A') + i)}. {opt}")
            lines.append("")
        if len(bank.questions) > 10:
            lines.append(f"…还有 {len(bank.questions) - 10} 题")
        QMessageBox.information(self, "题库预览", "\n".join(lines))

    # ---------------------------------------------------------------- 刷新

    def refresh(self) -> None:
        self.table.setRowCount(0)
        for exam_id, bank in sorted(self.state.banks.items()):
            r = self.table.rowCount()
            self.table.insertRow(r)
            for c, val in enumerate(
                [exam_id, bank.title, str(len(bank.questions)), str(bank.pick)]
            ):
                item = QTableWidgetItem(val)
                if c == 2 or c == 3:
                    item.setTextAlignment(Qt.AlignmentFlag.AlignCenter)
                self.table.setItem(r, c, item)
