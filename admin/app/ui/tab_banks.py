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

from app.core.bank_import import load_paths
from app.core.state import ProjectState
from app.ui.import_dialog import ImportConfirmDialog


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
        """导入题库：支持本程序格式，也支持外部的 questions/answers 成对文件。"""
        paths, _ = QFileDialog.getOpenFileNames(
            self,
            "选择题库文件（可多选；外部题库请同时选中 .questions 与 .answers）",
            "",
            "JSON (*.json)",
        )
        if not paths:
            return

        loaded, errors, warns = load_paths([Path(p) for p in paths])

        # 导入前确认：允许改题库 ID 与显示名称（文件名往往是 choice/fill 这种通用词）
        if loaded:
            dlg = ImportConfirmDialog(loaded, self)
            if dlg.exec() != dlg.DialogCode.Accepted:
                return

        incoming = [x.bank for x in loaded]

        # 冲突确认：同一 exam_id 已存在时**不能静默覆盖**。
        conflicts = self.state.conflicting_bank_ids(incoming)
        overwrite = False
        if conflicts:
            shown = "\n".join(f"· {c}" for c in conflicts[:10])
            more = f"\n… 共 {len(conflicts)} 个" if len(conflicts) > 10 else ""
            ret = QMessageBox.question(
                self,
                "题库已存在",
                f"以下 exam_id 已存在（或本批内重复）：\n\n{shown}{more}\n\n"
                "选择「是」覆盖已有题库，选择「否」跳过这些题库。",
                QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
            )
            overwrite = ret == QMessageBox.StandardButton.Yes

        added, replaced, skipped = self.state.merge_banks(incoming, overwrite=overwrite)
        for item in loaded:
            if item.bank.exam_id in self.state.banks:
                self.state.bank_paths[item.bank.exam_id] = str(item.source.resolve())
        ok = added + replaced

        self.refresh()
        if ok or skipped:
            self.changed.emit()

        detail = f"新增 {added} 个"
        if replaced:
            detail += f"，覆盖 {replaced} 个"
        if skipped:
            detail += f"，跳过 {skipped} 个（已存在）"

        if errors:
            QMessageBox.warning(
                self,
                f"导入完成：成功 {ok} 个，失败 {len(errors)} 个",
                "\n\n".join(errors),
            )
        elif warns:
            # 有提示（例如外部题库缺 judge 判分材料）：仍算成功，但要说清楚
            QMessageBox.information(
                self,
                "导入完成（有提示）",
                detail + "。\n\n" + "\n".join(warns[:10]),
            )
        else:
            QMessageBox.information(self, "导入完成", detail + "。")

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
            lines.extend(_format_stem(q.stem, prefix=f"[{q.qtype}] "))
            # 按正文形态展示：模板/代码题若只打 options，预览里看起来像空题
            if q.body.kind == "options":
                for opt in q.body.options:
                    lines.append(f"    {opt.key}. {opt.text}")
            elif q.body.kind == "template":
                lines.append(f"    （{q.body.blank_count} 处空白）")
                lines.extend(f"    | {ln}" for ln in (q.body.template or "").splitlines())
            elif q.body.kind == "code":
                lines.append(f"    （{q.body.blank_count} 处错误）")
                lines.extend(f"    {i + 1:>3}| {ln}" for i, ln in enumerate(q.body.code))
            else:
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


def _format_stem(stem: str, prefix: str = "") -> list[str]:
    """把题干按 Markdown 子集拆成便于阅读的纯文本预览。

    与学生端保持一致（见 docs/题库JSON规范.md）：
    - 围栏里的内容用 "| " 前缀显示成代码块；
    - 行内代码保留反引号（教师能看出这是代码）；
    - 加粗去掉 ** 标记；
    - 图片（data URI）显示成 [图片 说明]，绝不把几百 KB 的 base64 打进预览框。
    """
    import re

    # ![alt](data:image/...) → [图片 alt]
    stem = re.sub(r"!\[([^\]]*)\]\(data:image/[^)]*\)", r"[图片 \1]", stem)
    stem = stem.replace("**", "")

    out: list[str] = []
    in_code = False
    started = False
    for line in stem.replace("\r\n", "\n").replace("\r", "\n").split("\n"):
        if line.lstrip().startswith("```"):
            in_code = not in_code
            continue
        if in_code:
            out.append(f"    | {line}")
        elif not started:
            out.append(f"{prefix}{line}")
            started = True
        else:
            out.append(f"    {line}")
    return out or [prefix.rstrip()]
