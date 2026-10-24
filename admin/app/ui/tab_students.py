"""学生 Tab：全局学号名单 + 班级分组。

## 为什么班级只是"全局名单的分组"

学生端只认 manifest 里的学号列表，班级是管理端的概念。如果让班级各自持有
一份名单，同一个学生在两个班就会出现两套记录，补人/删人时必然对不上。
所以这里保持一条硬不变量：**班级成员 ⊆ 全局名单**。

- 「全部学生」= 编辑全局名单（原有行为）
- 「未分班」= 只读视图：还在全局名单里、但没分到任何班的学生
- 某个班 = 编辑该班成员；新增的学号会自动并入全局名单

## 校验策略

输入时只做**轻量提示**（非数字、重复、为 0 计数），
真正写入 `ProjectState` 时才走严格校验——否则每敲一个字符就弹错误框。
"""

from __future__ import annotations

from PySide6.QtCore import Qt, Signal
from PySide6.QtWidgets import (
    QFileDialog,
    QFormLayout,
    QHBoxLayout,
    QInputDialog,
    QLabel,
    QLineEdit,
    QListWidget,
    QListWidgetItem,
    QMessageBox,
    QPlainTextEdit,
    QPushButton,
    QSplitter,
    QVBoxLayout,
    QWidget,
)

from app.core.state import ProjectState

# 「未分班」在列表里的标识；None 表示「全部学生」
UNASSIGNED = "\x00unassigned"


class StudentsTab(QWidget):
    changed = Signal()

    def __init__(self, state: ProjectState) -> None:
        super().__init__()
        self.state = state
        #: None = 全部学生；UNASSIGNED = 未分班（只读）；其它 = 班级名
        self.current: str | None = None
        self._build_ui()
        self.refresh()

    # ---------------------------------------------------------------- UI

    def _build_ui(self) -> None:
        lay = QVBoxLayout(self)

        tip = QLabel(
            "每行一个学号（也支持逗号/空格分隔）。学号必须是正整数。\n"
            "可以直接从 Excel 复制一列粘贴进来；也可以按班级分组管理。"
        )
        tip.setWordWrap(True)
        lay.addWidget(tip)

        split = QSplitter(Qt.Orientation.Horizontal)

        left = QWidget()
        ll = QVBoxLayout(left)
        ll.setContentsMargins(0, 0, 0, 0)
        ll.addWidget(QLabel("名单 / 班级"))
        self.list = QListWidget()
        self.list.currentItemChanged.connect(self._on_select)
        ll.addWidget(self.list, 1)
        self.btn_new = QPushButton("新建班级…")
        self.btn_new.clicked.connect(self.new_class)
        ll.addWidget(self.btn_new)
        split.addWidget(left)

        right = QWidget()
        rl = QVBoxLayout(right)
        rl.setContentsMargins(0, 0, 0, 0)

        form = QFormLayout()
        self.edit_name = QLineEdit()
        self.edit_name.setPlaceholderText("例如：计算机 2021-1 班")
        self.edit_code = QLineEdit()
        self.edit_code.setPlaceholderText("可选，例如 CS2101")
        form.addRow("班级名", self.edit_name)
        form.addRow("编号", self.edit_code)
        rl.addLayout(form)

        rl.addWidget(QLabel("成员（每行一个学号）"))
        self.edit = QPlainTextEdit()
        self.edit.setPlaceholderText("2021001\n2021002\n2021003")
        self.edit.textChanged.connect(self._on_text_changed)
        rl.addWidget(self.edit, 1)

        bar = QHBoxLayout()
        self.btn_import = QPushButton("从 CSV 导入…")
        self.btn_import.clicked.connect(self.import_csv)
        self.btn_apply = QPushButton("应用到名单")
        self.btn_apply.clicked.connect(self.apply_to_state)
        self.btn_delete = QPushButton("删除班级")
        self.btn_delete.clicked.connect(self.delete_class)
        bar.addWidget(self.btn_import)
        bar.addWidget(self.btn_apply)
        bar.addWidget(self.btn_delete)
        bar.addStretch(1)
        rl.addLayout(bar)

        self.stats = QLabel()
        rl.addWidget(self.stats)

        split.addWidget(right)
        split.setStretchFactor(0, 0)
        split.setStretchFactor(1, 1)
        split.setSizes([240, 700])
        lay.addWidget(split, 1)

    # ---------------------------------------------------------------- 列表

    def refresh(self) -> None:
        """重建左侧列表（保留当前选择）。"""
        keep = self.current
        self.list.blockSignals(True)
        self.list.clear()
        self._add_item("全部学生", None, len(self.state.students))
        unassigned = [s for s in self.state.students if not self.state.classes_of(s)]
        self._add_item("未分班", UNASSIGNED, len(unassigned))
        for c in self.state.classes:
            label = (
                f"{c.name}（{len(c.students)}）"
                if not c.code
                else f"{c.name} [{c.code}]（{len(c.students)}）"
            )
            self._add_item(label, c.name, len(c.students))
        self.list.blockSignals(False)

        row = 0
        for i in range(self.list.count()):
            if self.list.item(i).data(Qt.ItemDataRole.UserRole) == keep:
                row = i
                break
        self.list.setCurrentRow(row)

    def _add_item(self, text: str, key: str | None, _count: int) -> None:
        item = QListWidgetItem(text)
        item.setData(Qt.ItemDataRole.UserRole, key)
        self.list.addItem(item)

    def _on_select(self, item: QListWidgetItem | None, _prev) -> None:
        if item is None:
            return
        self.current = item.data(Qt.ItemDataRole.UserRole)
        is_class = self.current not in (None, UNASSIGNED)
        self.edit_name.setEnabled(is_class)
        self.edit_code.setEnabled(is_class)
        self.btn_delete.setEnabled(is_class)
        self.btn_apply.setEnabled(self.current != UNASSIGNED)
        # 未分班是只读视图：导入进来的内容既不能应用、切走又会丢
        self.btn_import.setEnabled(self.current != UNASSIGNED)

        if self.current is None:
            self.edit_name.setText("")
            self.edit_code.setText("")
            self.edit.setReadOnly(False)
            self.edit.setPlainText("\n".join(str(s) for s in self.state.students))
        elif self.current == UNASSIGNED:
            self.edit_name.setText("（未分班）")
            self.edit_code.setText("")
            self.edit.setReadOnly(True)
            self.edit.setPlainText(
                "\n".join(str(s) for s in self.state.students if not self.state.classes_of(s))
            )
        else:
            c = self.state.class_by_name(self.current)
            if c is None:
                return
            self.edit_name.setText(c.name)
            self.edit_code.setText(c.code)
            self.edit.setReadOnly(False)
            self.edit.setPlainText("\n".join(str(s) for s in c.students))
        self._on_text_changed()

    # ---------------------------------------------------------------- 解析

    def _parse(self) -> tuple[list[int], list[str]]:
        """解析文本框内容，返回 (合法学号列表, 问题描述列表)。"""
        raw = self.edit.toPlainText()
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

    def _on_text_changed(self) -> None:
        good, problems = self._parse()
        total = len(good) + len(problems)
        msg = f"共 {total} 项，有效 {len(good)} 个"
        if problems:
            msg += f"，有问题 {len(problems)} 项：" + "；".join(problems[:3])
            if len(problems) > 3:
                msg += " …"
        if self.current not in (None, UNASSIGNED):
            msg += f"　|　共 {len(self.state.classes)} 个班级"
        self.stats.setText(msg)

    # ---------------------------------------------------------------- 操作

    def new_class(self) -> None:
        name, ok = QInputDialog.getText(self, "新建班级", "班级名（例如：计算机 2021-1 班）")
        if not ok or not name.strip():
            return
        try:
            c = self.state.add_class(name)
        except ValueError as e:
            QMessageBox.warning(self, "无法新建", str(e))
            return
        self.current = c.name
        self.changed.emit()
        self.refresh()
        self.edit_name.setFocus()

    def delete_class(self) -> None:
        if self.current in (None, UNASSIGNED):
            return
        name = self.current
        ret = QMessageBox.question(
            self,
            "删除班级",
            f"删除班级「{name}」？\n成员仍会保留在学生名单里，只是不再属于这个班。",
        )
        if ret != QMessageBox.StandardButton.Yes:
            return
        self.state.remove_class(name)
        self.current = None
        self.changed.emit()
        self.refresh()

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
        if self.current == UNASSIGNED:
            return
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

        if self.current is None:
            self._apply_global(good)
            return

        name = self.current
        c = self.state.class_by_name(name)
        if c is None:
            return

        # **先**校验新名字，再保存成员：否则会出现"名字没改成、成员却已写进去"，
        # 老师会以为整页都没保存。
        new_name = self.edit_name.text().strip()
        if new_name and new_name != c.name and self.state.class_by_name(new_name) is not None:
            QMessageBox.warning(
                self, "无法保存", f"班级名重复：{new_name}\n本页没有保存，请换一个名字。"
            )
            return

        self.state.set_class_members(name, good)
        if new_name and new_name != c.name:
            c.name = new_name
            self.current = new_name
        c.code = self.edit_code.text().strip()
        self.changed.emit()
        self.refresh()
        QMessageBox.information(
            self,
            "已保存",
            f"班级「{self.current}」现有 {len(good)} 人。新学号已自动加入学生名单。",
        )

    def _apply_global(self, good: list[int]) -> None:
        # 同步到所有"使用全部学生"的场次。
        #
        # 场次里的 students 是**创建时的快照**，不随全局名单变化。
        # 按"原名单 == 旧全局名单"判定它当初用的是全部学生。
        old = set(self.state.students)
        synced = 0
        if old:
            for s in self.state.sessions:
                if set(s.students) == old:
                    s.students = list(good)
                    synced += 1
        self.state.set_students(good)
        self.changed.emit()
        self.refresh()

        tail = f"\n已同步 {synced} 个使用「全部学生」的场次。" if synced else ""
        QMessageBox.information(self, "已应用", f"学号名单已更新为 {len(good)} 人。{tail}")

    def _load_from_state(self) -> None:
        self.refresh()
