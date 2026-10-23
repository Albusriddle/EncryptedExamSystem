"""场次 Tab：编排考试时间、绑定题库、看清时间窗重叠。

## 时间轴为什么重要

Rust 端 `check_student_overlap` 会拒绝"同一学生同时在两个重叠场次"，
但教师只有在**点打包时**才会撞到这个错误。时间轴让他在编排时就看到红色重叠，
并且下方会复刻与 Rust 端一致的判断，提前给出同样的提示。
"""

from __future__ import annotations

from PySide6.QtCore import QDateTime, QRectF, Qt, Signal
from PySide6.QtGui import QBrush, QColor, QFont, QPainter, QPen
from PySide6.QtWidgets import (
    QCheckBox,
    QComboBox,
    QDateTimeEdit,
    QDoubleSpinBox,
    QFormLayout,
    QGroupBox,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QListWidget,
    QListWidgetItem,
    QMessageBox,
    QPushButton,
    QSpinBox,
    QSplitter,
    QVBoxLayout,
    QWidget,
)

from app.core.models import Session, SessionExam
from app.core.state import ProjectState, match_bank_exam_id
from app.core.timeparse import local_to_rfc3339, parse_rfc3339

# 重叠场次的配色
COLOR_OK = QColor(70, 130, 180)
COLOR_OVERLAP = QColor(200, 60, 60)
COLOR_AXIS = QColor(120, 120, 120)


class TimelineWidget(QWidget):
    """24 小时时间轴。横轴固定为 0:00–24:00，重叠场次描红。"""

    def __init__(self) -> None:
        super().__init__()
        self.sessions: list[Session] = []
        self.overlap_ids: set[int] = set()
        self.setMinimumHeight(110)

    def set_sessions(self, sessions: list[Session], overlap_ids: set[int]) -> None:
        self.sessions = sessions
        self.overlap_ids = overlap_ids
        self.update()

    def paintEvent(self, event) -> None:  # noqa: N802 - Qt 命名
        p = QPainter(self)
        p.setRenderHint(QPainter.RenderHint.Antialiasing)

        margin_left, margin_right, margin_top = 44, 8, 18
        w = self.width() - margin_left - margin_right
        h = self.height() - margin_top - 18
        if w <= 0 or h <= 0:
            return

        # 坐标轴（0:00 – 24:00）
        p.setPen(QPen(COLOR_AXIS, 1))
        p.drawLine(margin_left, margin_top + h, margin_left + w, margin_top + h)
        f = QFont()
        f.setPointSize(8)
        p.setFont(f)
        for hour in range(0, 25, 3):
            x = margin_left + w * hour / 24.0
            p.drawLine(int(x), margin_top + h, int(x), margin_top + h + 4)
            p.drawText(int(x) - 8, margin_top + h + 16, f"{hour:02d}:00")

        # 每条场次一行
        row_h = min(22, max(14, h / max(len(self.sessions), 1) - 4)) if self.sessions else 18
        for i, s in enumerate(self.sessions):
            try:
                start = parse_rfc3339(s.start)
                end = parse_rfc3339(s.end)
            except ValueError:
                continue
            # 只画"当天"的部分：跨天场次必须**真的截断到本图**。
            # 旧实现注释写了"截断"却没做：22:00→次日 02:00 的场次
            # 会算出 26:00 的坐标，矩形直接画到控件外面。
            day_start = start - (start % 86400)
            right = margin_left + w
            x0 = margin_left + w * ((start - day_start) / 86400.0)
            x1 = margin_left + w * ((end - day_start) / 86400.0)
            x0 = min(max(x0, margin_left), right)
            x1 = min(max(x1, x0 + 3), right)

            y = margin_top + 2 + i * (row_h + 3)
            rect = QRectF(x0, y, x1 - x0, row_h)
            color = COLOR_OVERLAP if s.session_id in self.overlap_ids else COLOR_OK
            p.setPen(QPen(color.darker(130), 1))
            p.setBrush(
                QBrush(
                    color.lighter(150)
                    if s.session_id not in self.overlap_ids
                    else QColor(255, 210, 210)
                )
            )
            p.drawRoundedRect(rect, 3, 3)

            p.setPen(QPen(QColor(40, 40, 40), 1))
            label = f"#{s.session_id}"
            if rect.width() > 40:
                label += f" {s.start[11:16]}–{s.end[11:16]}"
            p.drawText(rect.adjusted(4, 0, -2, 0), Qt.AlignmentFlag.AlignVCenter, label)


class SessionsTab(QWidget):
    changed = Signal()

    def __init__(self, state: ProjectState) -> None:
        super().__init__()
        self.state = state
        self._current: Session | None = None
        self._build_ui()
        # 必须建出科目勾选框：否则"打开工程"后（题库已存在）
        # 这一栏会是空的，教师看不到任何可勾的科目。
        self.refresh_choices()
        # 表单预填一个合法草稿（时间=下一个整点起两小时），
        # 但不加入 state、也不占用 _current：教师点保存时才真正创建。
        self._load_into_form(self.state.new_draft_session(), set_current=False)

    # ---------------------------------------------------------------- 构建

    def _build_ui(self) -> None:
        lay = QVBoxLayout(self)

        split = QSplitter(Qt.Orientation.Horizontal)

        # 左侧：场次列表
        left = QWidget()
        ll = QVBoxLayout(left)
        ll.setContentsMargins(0, 0, 0, 0)
        self.list = QListWidget()
        self.list.currentItemChanged.connect(self._on_select)
        ll.addWidget(self.list, 1)
        row = QHBoxLayout()
        self.btn_new = QPushButton("新建")
        self.btn_new.clicked.connect(self.new_session)
        self.btn_del = QPushButton("删除")
        self.btn_del.clicked.connect(self.delete_session)
        row.addWidget(self.btn_new)
        row.addWidget(self.btn_del)
        ll.addLayout(row)
        split.addWidget(left)

        # 右侧：编辑表单
        right = QWidget()
        rl = QVBoxLayout(right)
        rl.setContentsMargins(0, 0, 0, 0)

        form_box = QGroupBox("场次设置")
        form = QFormLayout(form_box)

        self.spin_id = QSpinBox()
        self.spin_id.setRange(1, 2**31 - 1)
        form.addRow("场次编号", self.spin_id)

        self.dt_start = QDateTimeEdit()
        self.dt_start.setDisplayFormat("yyyy-MM-dd HH:mm")
        self.dt_start.setCalendarPopup(True)
        form.addRow("开始时间", self.dt_start)

        self.dt_end = QDateTimeEdit()
        self.dt_end.setDisplayFormat("yyyy-MM-dd HH:mm")
        self.dt_end.setCalendarPopup(True)
        form.addRow("结束时间", self.dt_end)

        self.combo_roster = QComboBox()
        self.combo_roster.addItem("使用全部学生", "all")
        self.combo_roster.addItem("自定义名单", "custom")
        self.combo_roster.currentIndexChanged.connect(self._on_roster_mode)
        form.addRow("学号名单", self.combo_roster)

        self.edit_roster = QLineEdit()
        self.edit_roster.setPlaceholderText("逗号或空格分隔，例如 2021001 2021002")
        self.edit_roster.setEnabled(False)
        form.addRow("自定义学号", self.edit_roster)

        rl.addWidget(form_box)

        # 科目
        subj_box = QGroupBox("考试科目")
        sl = QVBoxLayout(subj_box)
        self.checks: list[tuple[QCheckBox, QDoubleSpinBox, str]] = []
        self.combo_holder = QVBoxLayout()
        sl.addLayout(self.combo_holder)
        rl.addWidget(subj_box)

        # 保存按钮
        save_row = QHBoxLayout()
        self.btn_save = QPushButton("保存此场次")
        self.btn_save.clicked.connect(self.save_session)
        save_row.addWidget(self.btn_save)
        save_row.addStretch(1)
        rl.addLayout(save_row)

        self.problems_label = QLabel()
        self.problems_label.setWordWrap(True)
        self.problems_label.setStyleSheet("color: #b03030;")
        rl.addWidget(self.problems_label)
        rl.addStretch(1)

        split.addWidget(right)
        split.setStretchFactor(0, 1)
        split.setStretchFactor(1, 2)
        lay.addWidget(split, 3)

        # 底部：时间轴
        axis_box = QGroupBox("时间轴（重叠场次标红）")
        al = QVBoxLayout(axis_box)
        self.timeline = TimelineWidget()
        al.addWidget(self.timeline)
        lay.addWidget(axis_box, 1)

    # ---------------------------------------------------------------- 刷新

    def refresh_choices(self) -> None:
        """题库/学生变化后重建科目勾选框。

        重建时必须**保留用户已有的勾选与抽题数**：
        勾选框是重新 new 出来的控件，天然处于未勾选状态，而 `refresh()`
        用的是 `blockSignals(True)`，不会触发 `_on_select` 去重新加载表单。
        结果就是——保存一次（会 emit changed → 本函数）之后，
        界面上的勾选全部消失，但数据其实还在模型里。
        """
        # 重建前快照：exam_id -> (是否勾选, 抽题数)
        prev: dict[str, tuple[bool, int]] = {
            exam_id: (cb.isChecked(), int(pick.value())) for cb, pick, exam_id in self.checks
        }

        # 清空旧控件
        while self.combo_holder.count():
            item = self.combo_holder.takeAt(0)
            w = item.widget()
            if w is not None:
                w.deleteLater()
        self.checks = []

        for exam_id, bank in sorted(self.state.banks.items()):
            row = QWidget()
            hl = QHBoxLayout(row)
            hl.setContentsMargins(0, 0, 0, 0)
            cb = QCheckBox(f"{bank.title}（{exam_id}，题量 {len(bank.questions)}）")
            pick = QDoubleSpinBox()
            pick.setDecimals(0)
            pick.setRange(1, max(len(bank.questions), 1))
            pick.setValue(min(bank.pick, len(bank.questions)))
            pick.setFixedWidth(70)
            hl.addWidget(cb)
            hl.addWidget(QLabel("抽题数"))
            hl.addWidget(pick)
            hl.addStretch(1)
            self.combo_holder.addWidget(row)
            self.checks.append((cb, pick, exam_id))

        # 恢复快照；新增的题库没有快照，保持默认（未勾选）。
        for cb, pick, exam_id in self.checks:
            if exam_id in prev:
                checked, n = prev[exam_id]
                cb.setChecked(checked)
                pick.setValue(min(n, int(pick.maximum())))

        self.refresh()

    def refresh(self) -> None:
        # 列表
        cur_id = self._current.session_id if self._current else None
        self.list.blockSignals(True)
        self.list.clear()
        for s in sorted(self.state.sessions, key=lambda x: x.session_id):
            item = QListWidgetItem(f"#{s.session_id}   {s.start} → {s.end}")
            item.setData(Qt.ItemDataRole.UserRole, s.session_id)
            self.list.addItem(item)
            if s.session_id == cur_id:
                self.list.setCurrentItem(item)
        self.list.blockSignals(False)

        # 时间轴 + 重叠提示
        overlap = self._overlap_pairs()
        overlap_ids = {sid for pair in overlap for sid in pair}
        self.timeline.set_sessions(self.state.sessions, overlap_ids)

        problems = self.state.validate()
        self.problems_label.setText("\n".join(f"⚠ {p}" for p in problems) if problems else "")

    def _overlap_pairs(self) -> list[tuple[int, int]]:
        out: list[tuple[int, int]] = []
        ss = self.state.sessions
        for i in range(len(ss)):
            for j in range(i + 1, len(ss)):
                a, b = ss[i], ss[j]
                try:
                    if not (
                        parse_rfc3339(a.start) < parse_rfc3339(b.end)
                        and parse_rfc3339(b.start) < parse_rfc3339(a.end)
                    ):
                        continue
                except ValueError:
                    continue
                out.append((a.session_id, b.session_id))
        return out

    # ---------------------------------------------------------------- 交互

    def _on_select(self, cur: QListWidgetItem | None, _prev) -> None:
        if cur is None:
            return
        sid = cur.data(Qt.ItemDataRole.UserRole)
        s = next((x for x in self.state.sessions if x.session_id == sid), None)
        if s is None:
            return
        self._load_into_form(s)

    def _load_into_form(self, s: Session, *, set_current: bool = True) -> None:
        """把某个场次的字段填进右侧表单。

        `refresh()` 里是 `blockSignals(True)` 设置选中项的，**不会**触发
        `_on_select`；若只依赖信号，新建/加载工程后表单会停在 Qt 默认值
        （2000-01-01），保存时报"开始时间必须早于结束时间"。
        因此新建之后必须显式调用本方法。
        """
        if set_current:
            self._current = s
        self.spin_id.setValue(s.session_id)
        try:
            self.dt_start.setDateTime(QDateTime.fromSecsSinceEpoch(parse_rfc3339(s.start)))
            self.dt_end.setDateTime(QDateTime.fromSecsSinceEpoch(parse_rfc3339(s.end)))
        except ValueError:
            pass

        if set(s.students) == set(self.state.students) and self.state.students:
            self.combo_roster.setCurrentIndex(0)
        else:
            self.combo_roster.setCurrentIndex(1)
            self.edit_roster.setText(" ".join(str(x) for x in s.students))

        # 勾选本场次的科目。
        #
        # 必须用 match_bank_exam_id 归一，不能只比 "exam_id" 与 "exam_id.json"：
        # 清单里的 file 允许写成 "demo/math.json"（Rust 端三层回退明确支持），
        # 旧写法遇到这种引用会一个都匹配不上 —— 界面显示"没选科目"，
        # 教师一保存就把 exams 清空，科目被静默丢掉。
        resolved: dict[str, SessionExam] = {}
        for e in s.exams:
            key = match_bank_exam_id(e.file, self.state.banks)
            if key is not None:
                resolved[key] = e
        for cb, pick, exam_id in self.checks:
            e = resolved.get(exam_id)
            cb.setChecked(e is not None)
            if e is not None and e.pick:
                pick.setValue(e.pick)

    def _on_roster_mode(self) -> None:
        self.edit_roster.setEnabled(self.combo_roster.currentData() == "custom")
        if self.combo_roster.currentData() == "all" and not self.edit_roster.text():
            self.edit_roster.setText(" ".join(str(x) for x in self.state.students))

    def new_session(self) -> None:
        s = self.state.new_draft_session()
        self.state.sessions.append(s)
        self.refresh()
        # 必须显式填充：refresh 里的选中是 blockSignals 的，不会触发 _on_select
        self._load_into_form(s)
        self.changed.emit()

    def delete_session(self) -> None:
        if self._current is None:
            return
        ret = QMessageBox.question(self, "确认删除", f"删除场次 #{self._current.session_id}？")
        if ret != QMessageBox.StandardButton.Yes:
            return
        self.state.remove_session(self._current.session_id)
        self._current = None
        self.refresh()
        self.changed.emit()

    def save_session(self) -> None:
        # 没选中任何场次时，把表单内容当作"新建"保存——
        # 旧实现直接拒绝（"请先选中一个场次"），但此时表单明明填好了，
        # 教师会以为程序坏了。
        creating = self._current is None

        # 学号
        if self.combo_roster.currentData() == "all":
            students = list(self.state.students)
        else:
            students = []
            for tok in self.edit_roster.text().replace(",", " ").split():
                if tok.isdigit() and int(tok) > 0:
                    students.append(int(tok))
                else:
                    QMessageBox.warning(self, "学号非法", f"无法识别: {tok!r}")
                    return

        # 科目
        exams: list[SessionExam] = []
        for cb, pick, exam_id in self.checks:
            if cb.isChecked():
                exams.append(SessionExam(file=f"{exam_id}.json", pick=int(pick.value())))

        try:
            s = Session(
                session_id=int(self.spin_id.value()),
                start=local_to_rfc3339(self.dt_start.dateTime().toString("yyyy-MM-dd HH:mm")),
                end=local_to_rfc3339(self.dt_end.dateTime().toString("yyyy-MM-dd HH:mm")),
                students=students,
                exams=exams,
            )
        except Exception as e:  # noqa: BLE001 - pydantic 校验错误原样展示
            QMessageBox.critical(self, "场次校验失败", str(e))
            return

        # 编号冲突检查（改编号时）
        clash = [
            x
            for x in self.state.sessions
            if x.session_id == s.session_id and self._current is not None and x is not self._current
        ]
        if clash:
            QMessageBox.critical(self, "编号冲突", f"场次编号 {s.session_id} 已被占用。")
            return

        if creating:
            # 新建：编号可能已被占用，重新分配
            if any(x.session_id == s.session_id for x in self.state.sessions):
                s = s.model_copy(update={"session_id": self.state.next_session_id()})
            self.state.sessions.append(s)
            # 注意：这里不设 _current，交给下面的 _load_into_form
        else:
            self.state.upsert_session(s)
        self.refresh()
        self._load_into_form(s)
        self.changed.emit()
        QMessageBox.information(self, "已保存", f"场次 #{s.session_id} 已保存。")
