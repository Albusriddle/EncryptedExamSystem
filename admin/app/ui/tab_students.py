"""学生 Tab：粘贴学号列表。

## 为什么用多行文本框而不是表格

教师手上的学号通常是从教务系统导出的，直接粘贴最省事。
表格要处理增删行、单元格编辑，反而更麻烦。

## 校验策略

输入时只做**轻量提示**（非数字、重复、为 0 标黄并计数），
真正写入 `ProjectState` 时才走 pydantic 严格校验——
否则每敲一个字符就弹错误框，没法用。
"""

from __future__ import annotations

from PySide6.QtCore import Signal
from PySide6.QtWidgets import (
    QFileDialog,
    QHBoxLayout,
    QLabel,
    QMessageBox,
    QPlainTextEdit,
    QPushButton,
    QVBoxLayout,
    QWidget,
)

from app.core.state import ProjectState


class StudentsTab(QWidget):
    changed = Signal()

    def __init__(self, state: ProjectState) -> None:
        super().__init__()
        self.state = state
        self._build_ui()
        self._load_from_state()

    def _build_ui(self) -> None:
        lay = QVBoxLayout(self)

        tip = QLabel(
            "每行一个学号（也支持逗号/空格分隔）。学号必须是正整数。\n"
            "可以直接从 Excel 复制一列粘贴进来。"
        )
        tip.setWordWrap(True)
        lay.addWidget(tip)

        self.edit = QPlainTextEdit()
        self.edit.setPlaceholderText("2021001\n2021002\n2021003")
        self.edit.textChanged.connect(self._on_text_changed)
        lay.addWidget(self.edit, 1)

        bar = QHBoxLayout()
        self.btn_import = QPushButton("从 CSV 导入…")
        self.btn_import.clicked.connect(self.import_csv)
        self.btn_apply = QPushButton("应用为学号名单")
        self.btn_apply.clicked.connect(self.apply_to_state)
        bar.addWidget(self.btn_import)
        bar.addWidget(self.btn_apply)
        bar.addStretch(1)
        lay.addLayout(bar)

        self.stats = QLabel()
        lay.addWidget(self.stats)

    # ---------------------------------------------------------------- 解析

    def _parse(self) -> tuple[list[int], list[str]]:
        """解析文本框内容，返回 (合法学号列表, 问题描述列表)。"""
        raw = self.edit.toPlainText()
        # 逗号、中文逗号、空白都当分隔符
        tokens: list[str] = []
        for chunk in raw.replace("，", ",").replace(",", " ").split():
            if chunk:
                tokens.append(chunk)

        good: list[int] = []
        problems: list[str] = []
        seen: dict[int, int] = {}
        for tok in tokens:
            if not tok.isdigit():
                problems.append(f"非数字: {tok!r}")
                continue
            sid = int(tok)
            if sid <= 0:
                problems.append(f"必须是正整数: {tok}")
                continue
            if sid in seen:
                problems.append(f"重复: {sid}")
                continue
            seen[sid] = 1
            good.append(sid)
        return good, problems

    # ---------------------------------------------------------------- 交互

    def _on_text_changed(self) -> None:
        good, problems = self._parse()
        total = len(good) + len(problems)
        msg = f"共 {total} 项，有效 {len(good)} 个"
        if problems:
            msg += f"，有问题 {len(problems)} 项：" + "；".join(problems[:3])
            if len(problems) > 3:
                msg += " …"
        self.stats.setText(msg)

    def import_csv(self) -> None:
        path, _ = QFileDialog.getOpenFileName(
            self, "选择 CSV", "", "CSV (*.csv *.txt);;所有文件 (*)"
        )
        if not path:
            return
        values: list[str] = []
        try:
            with open(path, encoding="utf-8-sig") as f:  # utf-8-sig 吃掉 BOM
                for line in f:
                    line = line.strip()
                    if not line:
                        continue
                    # 取第一列（逗号或制表符分隔）
                    first = line.replace("\t", ",").split(",")[0].strip()
                    if first:
                        values.append(first)
        except UnicodeDecodeError:
            QMessageBox.critical(
                self,
                "编码错误",
                "CSV 不是 UTF-8 编码。\n在 Excel 里「另存为 → CSV UTF-8（逗号分隔）」再试。",
            )
            return
        except OSError as e:
            QMessageBox.critical(self, "读取失败", str(e))
            return

        existing = self.edit.toPlainText().strip()
        addition = "\n".join(values)
        self.edit.setPlainText((existing + "\n" + addition).strip() if existing else addition)
        QMessageBox.information(self, "导入完成", f"已追加 {len(values)} 行。")

    def apply_to_state(self) -> None:
        good, problems = self._parse()
        if problems:
            QMessageBox.warning(
                self,
                "存在问题",
                "以下内容无法识别，请先修正：\n\n" + "\n".join(problems[:20]),
            )
            return
        if not good:
            QMessageBox.information(self, "提示", "学号列表为空。")
            return
        # 同步到所有"使用全部学生"的场次。
        #
        # 场次里的 students 是**创建时的快照**，不随全局名单变化。
        # 旧实现只改 self.state.students（注释却写着"同步"），
        # 教师中途补学生后，这些学生进不了考场，而且没有任何提示。
        # 这里按"原名单 == 旧全局名单"判定它当初用的是全部学生。
        old = set(self.state.students)
        synced = 0
        if old:
            for s in self.state.sessions:
                if set(s.students) == old:
                    s.students = list(good)
                    synced += 1
        self.state.students = good
        self.changed.emit()

        tail = f"\n已同步 {synced} 个使用「全部学生」的场次。" if synced else ""
        QMessageBox.information(self, "已应用", f"学号名单已更新为 {len(good)} 人。{tail}")

    def _load_from_state(self) -> None:
        self.edit.setPlainText("\n".join(str(s) for s in self.state.students))
