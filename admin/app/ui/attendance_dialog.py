"""缺考名单对话框：按场次列出已交/未交/缺考/失败/名单外。"""

from __future__ import annotations

import csv
from datetime import datetime

from PySide6.QtCore import Qt
from PySide6.QtGui import QGuiApplication
from PySide6.QtWidgets import (
    QCheckBox,
    QDialog,
    QFileDialog,
    QHBoxLayout,
    QLabel,
    QMessageBox,
    QPushButton,
    QTableWidget,
    QTableWidgetItem,
    QVBoxLayout,
)

from app.core.attendance import STATUS_LABELS, AttendanceResult, build_attendance, csv_rows
from app.core.models import Session, StudentClass


class AttendanceDialog(QDialog):
    def __init__(
        self,
        parent,
        sessions: list[Session],
        records: list[dict],
        classes: list[StudentClass],
    ) -> None:
        super().__init__(parent)
        self.sessions = sessions
        self.records = records
        self.classes = classes
        self.result: AttendanceResult | None = None

        self.setWindowTitle("缺考名单")
        self.resize(900, 620)
        lay = QVBoxLayout(self)

        self.chk_all = QCheckBox("包含没有答题卡的场次（会把该场全员标为缺考）")
        self.chk_all.toggled.connect(self._reload)
        lay.addWidget(self.chk_all)

        self.summary = QLabel("")
        self.summary.setWordWrap(True)
        lay.addWidget(self.summary)

        self.table = QTableWidget(0, 5)
        self.table.setHorizontalHeaderLabels(["场次", "学号", "班级", "状态", "说明"])
        self.table.setEditTriggers(QTableWidget.EditTrigger.NoEditTriggers)
        self.table.setSelectionBehavior(QTableWidget.SelectionBehavior.SelectRows)
        self.table.horizontalHeader().setStretchLastSection(True)
        lay.addWidget(self.table, 1)

        row = QHBoxLayout()
        self.btn_copy = QPushButton("复制本场缺考学号")
        self.btn_copy.clicked.connect(self._copy_missing)
        row.addWidget(self.btn_copy)
        self.btn_export = QPushButton("导出缺考名单…")
        self.btn_export.clicked.connect(self._export)
        row.addWidget(self.btn_export)
        row.addStretch(1)
        btn_close = QPushButton("关闭")
        btn_close.clicked.connect(self.accept)
        row.addWidget(btn_close)
        lay.addLayout(row)

        self._reload()

    def _reload(self) -> None:
        self.result = build_attendance(
            self.sessions,
            self.records,
            self.classes,
            include_empty_sessions=self.chk_all.isChecked(),
        )
        rows = [r for s in self.result.sessions for r in s.rows]
        self.table.setRowCount(len(rows))
        for i, r in enumerate(rows):
            values = [
                str(r.session_id),
                str(r.student_id),
                r.class_name,
                STATUS_LABELS[r.status],
                r.note,
            ]
            for col, text in enumerate(values):
                item = QTableWidgetItem(text)
                if r.status == "missing":
                    item.setForeground(Qt.GlobalColor.red)
                self.table.setItem(i, col, item)

        parts = []
        for s in self.result.sessions:
            parts.append(
                f"场次 {s.session_id}：应到 {s.roster} | 已交 {s.submitted} | "
                f"未交 {s.unsubmitted} | 缺考 {s.missing} | 失败 {s.invalid} | 名单外 {s.unlisted}"
            )
        if self.result.invalid_files:
            parts.append(f"另有 {len(self.result.invalid_files)} 个文件无法解析")
        self.summary.setText("\n".join(parts) if parts else "没有可统计的答题卡")

    def _copy_missing(self) -> None:
        row = self.table.currentRow()
        if row < 0:
            QMessageBox.information(self, "提示", "请先选中一行，复制该场的缺考学号。")
            return
        sid = int(self.table.item(row, 0).text())
        missing = [
            str(r.student_id)
            for s in (self.result.sessions if self.result else [])
            if s.session_id == sid
            for r in s.rows
            if r.status == "missing"
        ]
        if not missing:
            QMessageBox.information(self, "提示", f"场次 {sid} 没有缺考学生。")
            return
        QGuiApplication.clipboard().setText(" ".join(missing))
        QMessageBox.information(self, "已复制", f"场次 {sid} 的 {len(missing)} 个缺考学号已复制。")

    def _export(self) -> None:
        if self.result is None:
            return
        tag = datetime.now().strftime("%Y%m%d-%H%M%S")
        path, _ = QFileDialog.getSaveFileName(
            self, "导出缺考名单", f"缺考名单-{tag}.csv", "CSV (*.csv)"
        )
        if not path:
            return
        with open(path, "w", encoding="utf-8-sig", newline="") as f:
            writer = csv.writer(f)
            writer.writerow(["场次", "学号", "班级", "状态", "说明"])
            writer.writerows(csv_rows(self.result))
        QMessageBox.information(self, "完成", f"已导出到：\n{path}")
