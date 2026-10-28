"""场次 Tab：编排考试时间、绑定题库、看清时间窗重叠。

## 时间轴为什么重要

Rust 端 `check_student_overlap` 会拒绝"同一学生同时在两个重叠场次"，
但教师只有在**点打包时**才会撞到这个错误。时间轴让他在编排时就看到红色重叠，
并且下方会复刻与 Rust 端一致的判断，提前给出同样的提示。
"""

from __future__ import annotations

from dataclasses import dataclass

from PySide6.QtCore import QDateTime, QRectF, Qt, Signal
from PySide6.QtGui import QBrush, QColor, QFont, QPainter, QPen
from PySide6.QtWidgets import (
    QCheckBox,
    QComboBox,
    QDateTimeEdit,
    QDialog,
    QDialogButtonBox,
    QFormLayout,
    QGroupBox,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QListWidget,
    QListWidgetItem,
    QMessageBox,
    QPushButton,
    QScrollArea,
    QSpinBox,
    QSplitter,
    QVBoxLayout,
    QWidget,
)

from app.core.difficulty import check_difficulty_range
from app.core.models import Section, SectionPart, Session, SessionExam
from app.core.state import ProjectState, match_bank_exam_id
from app.core.timeparse import local_to_rfc3339, parse_rfc3339


@dataclass
class PartRow:
    """大题下的一个部分：标题、题数、精确难度。"""

    box: QWidget
    title: QLineEdit
    count: QSpinBox
    difficulty: QSpinBox
    hint: QLabel
    part_id: str
    up: QPushButton
    down: QPushButton


@dataclass
class SectionRow:
    """一个大题：标题、题库、若干部分、上下移动。"""

    box: QWidget
    title: QLineEdit
    bank: QComboBox
    parts_layout: QVBoxLayout
    parts: list[PartRow]
    section_id: str
    up: QPushButton
    down: QPushButton
    add_part: QPushButton
    #: 旧单层工程：未修改时保存仍写 parts=[]，不强制升级 v7。
    legacy_flat: bool = False
    was_legacy: bool = False


@dataclass
class BankRow:
    """场次里一行题库：勾选、抽题数、平均难度区间（可选）。"""

    cb: QCheckBox
    pick: QSpinBox
    limit: QCheckBox
    dmin: QSpinBox
    dmax: QSpinBox
    hint: QLabel
    exam_id: str


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
        # 高度必须随场次数增长：每行约 17pt（row_h 最小 14 + 间距 3），
        # 固定 110pt 时 10 个场次会被裁掉一半（考场编排正是 10 场量级）。
        self.setMinimumHeight(max(110, 26 + len(sessions) * 17))
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


def pick_width(max_value: int) -> int:
    """抽题数输入框的最小宽度（按最大值的位数算）。

    实测教训：固定 70px + QDoubleSpinBox 时，"500" 会显示成 "50"——
    教师以为抽 50 题，模型里其实是 500 题，考场上无法挽回。
    """
    digits = max(len(str(max(max_value, 1))), 2)
    return 50 + digits * 14


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
        self.combo_roster.addItem("按班级选择…", "by_class")
        self.combo_roster.addItem("自定义名单", "custom")
        self.combo_roster.currentIndexChanged.connect(self._on_roster_mode)
        form.addRow("学号名单", self.combo_roster)

        self.edit_roster = QLineEdit()
        self.edit_roster.setPlaceholderText("逗号或空格分隔，例如 2021001 2021002")
        self.edit_roster.textChanged.connect(self._update_roster_info)
        self.edit_roster.setEnabled(False)
        form.addRow("自定义学号", self.edit_roster)

        # 场次人数与来源：让老师一眼看出"是不是漏加了某个班"
        self.lbl_roster_info = QLabel("")
        self.lbl_roster_info.setStyleSheet("color: #888;")
        form.addRow("", self.lbl_roster_info)

        hint = QLabel("本场名单是保存时的快照；学生名单变更后请重新保存本场次。")
        hint.setStyleSheet("color: #888; font-size: 11px;")
        hint.setWordWrap(True)
        form.addRow("", hint)

        rl.addWidget(form_box)

        # 科目 / 大题
        subj_box = QGroupBox("考试科目 / 大题")
        sl = QVBoxLayout(subj_box)
        self.chk_sections = QCheckBox("按大题模式（每行固定题库、题数、精确难度；可上下调整顺序）")
        self.chk_sections.toggled.connect(self._on_sections_toggle)
        sl.addWidget(self.chk_sections)

        self.old_subject_widget = QWidget()
        old_subject_lay = QVBoxLayout(self.old_subject_widget)
        old_subject_lay.setContentsMargins(0, 0, 0, 0)
        self.checks: list[BankRow] = []
        self.combo_holder = QVBoxLayout()
        old_subject_lay.addLayout(self.combo_holder)
        sl.addWidget(self.old_subject_widget)

        self.sections_widget = QWidget()
        sec_lay = QVBoxLayout(self.sections_widget)
        sec_lay.setContentsMargins(0, 0, 0, 0)
        self.section_rows: list[SectionRow] = []
        self.sections_layout = QVBoxLayout()
        sec_lay.addLayout(self.sections_layout)
        self.btn_add_section = QPushButton("+ 新建大题")
        self.btn_add_section.clicked.connect(lambda: self._add_section_row())
        sec_lay.addWidget(self.btn_add_section)
        self.sections_widget.setVisible(False)
        sl.addWidget(self.sections_widget)
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

        # 右侧表单可滚动：大题/部分一多，窗口不应被无限撑长。
        right.setMinimumWidth(620)
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAlwaysOff)
        scroll.setWidget(right)
        split.addWidget(scroll)
        split.setStretchFactor(0, 1)
        split.setStretchFactor(1, 2)
        lay.addWidget(split, 3)

        # 底部：时间轴
        axis_box = QGroupBox("时间轴（重叠场次标红）")
        al = QVBoxLayout(axis_box)
        self.timeline = TimelineWidget()
        al.addWidget(self.timeline)
        lay.addWidget(axis_box, 1)

    # ---------------------------------------------------------------- 大题

    def _on_sections_toggle(self, on: bool) -> None:
        self.old_subject_widget.setVisible(not on)
        self.sections_widget.setVisible(on)
        if on and not self.section_rows:
            self._add_section_row()

    def _next_id(self, existing: set[str], prefix: str) -> str:
        n = 1
        while f"{prefix}{n}" in existing:
            n += 1
        return f"{prefix}{n}"

    def _fill_bank_combo(self, combo: QComboBox, sec: Section | None) -> None:
        combo.blockSignals(True)
        combo.clear()
        for exam_id, b in sorted(self.state.banks.items()):
            combo.addItem(b.title, exam_id)
        if sec:
            desired = match_bank_exam_id(sec.file, self.state.banks) or ""
            idx = combo.findData(desired)
            if idx >= 0:
                combo.setCurrentIndex(idx)
            else:
                combo.addItem(f"⚠ 原题库已删除：{sec.file}", "")
                combo.setCurrentIndex(combo.count() - 1)
        combo.blockSignals(False)

    def _add_section_row(self, sec: Section | None = None) -> None:
        box = QWidget()
        v = QVBoxLayout(box)
        v.setContentsMargins(0, 4, 0, 4)
        top = QHBoxLayout()
        title = QLineEdit(sec.title if sec else f"第{len(self.section_rows) + 1}大题")
        title.setMinimumWidth(100)
        bank = QComboBox()
        self._fill_bank_combo(bank, sec)
        up = QPushButton("↑")
        up.setFixedWidth(28)
        down = QPushButton("↓")
        down.setFixedWidth(28)
        delete = QPushButton("删除大题")
        top.addWidget(QLabel("大题"))
        top.addWidget(title)
        top.addWidget(QLabel("题库"))
        top.addWidget(bank, 1)
        top.addWidget(up)
        top.addWidget(down)
        top.addWidget(delete)
        v.addLayout(top)

        parts_layout = QVBoxLayout()
        v.addLayout(parts_layout)
        add_part = QPushButton("+ 新建部分")
        v.addWidget(add_part)

        existing = {r.section_id for r in self.section_rows}
        sid = sec.section_id if sec else self._next_id(existing, "s")
        legacy = bool(sec and not sec.parts)
        row = SectionRow(
            box,
            title,
            bank,
            parts_layout,
            [],
            sid,
            up,
            down,
            add_part,
            legacy_flat=legacy,
            was_legacy=legacy,
        )
        self.section_rows.append(row)
        self.sections_layout.addWidget(box)

        up.clicked.connect(lambda _b=False, r=row: self._move_section(r, -1))
        down.clicked.connect(lambda _b=False, r=row: self._move_section(r, 1))
        delete.clicked.connect(lambda _b=False, r=row: self._remove_section_row(r))
        add_part.clicked.connect(lambda _b=False, r=row: self._add_part_row(r))
        bank.currentIndexChanged.connect(lambda _i, r=row: self._refresh_part_hints(r))

        if sec and sec.parts:
            for part in sec.parts:
                self._add_part_row(row, part)
        else:
            # 旧单层大题自动视为一个部分
            self._add_part_row(
                row,
                SectionPart(
                    part_id=f"{sid}p1",
                    title="全部题目" if sec else "第1部分",
                    count=sec.count if sec else 1,
                    difficulty=sec.difficulty if sec else 1,
                )
                if sec
                else None,
            )

    def _add_part_row(self, section_row: SectionRow, part: SectionPart | None = None) -> None:
        box = QWidget()
        hl = QHBoxLayout(box)
        hl.setContentsMargins(24, 0, 0, 0)
        title = QLineEdit(part.title if part else f"第{len(section_row.parts) + 1}部分")
        title.setMinimumWidth(100)
        count = QSpinBox()
        count.setRange(1, 9999)
        count.setValue(part.count if part else 1)
        diff = QSpinBox()
        diff.setRange(1, 255)
        diff.setValue(part.difficulty if part else 1)
        hint = QLabel("")
        hint.setStyleSheet("color:#888;font-size:11px;")
        up = QPushButton("↑")
        up.setFixedWidth(28)
        down = QPushButton("↓")
        down.setFixedWidth(28)
        delete = QPushButton("删除部分")
        hl.addWidget(QLabel("部分"))
        hl.addWidget(title)
        hl.addWidget(QLabel("题数"))
        hl.addWidget(count)
        hl.addWidget(QLabel("难度"))
        hl.addWidget(diff)
        hl.addWidget(hint)
        hl.addWidget(up)
        hl.addWidget(down)
        hl.addWidget(delete)

        existing = {p.part_id for p in section_row.parts}
        pid = part.part_id if part else self._next_id(existing, f"{section_row.section_id}p")
        if part is None:
            # 新建部分才视为“升级为两层”；加载旧单层时补的隐式部分不算
            self._mark_section_custom(section_row)
        prow = PartRow(box, title, count, diff, hint, pid, up, down)
        section_row.parts.append(prow)
        section_row.parts_layout.addWidget(box)
        title.textChanged.connect(lambda _v, sec=section_row: self._mark_section_custom(sec))
        count.valueChanged.connect(
            lambda _v, r=prow, sec=section_row: (
                self._mark_section_custom(sec),
                self._update_part_hint(sec, r),
            )
        )
        diff.valueChanged.connect(
            lambda _v, r=prow, sec=section_row: (
                self._mark_section_custom(sec),
                self._update_part_hint(sec, r),
            )
        )
        up.clicked.connect(lambda _b=False, r=prow, sec=section_row: self._move_part(sec, r, -1))
        down.clicked.connect(lambda _b=False, r=prow, sec=section_row: self._move_part(sec, r, 1))
        delete.clicked.connect(
            lambda _b=False, r=prow, sec=section_row: self._remove_part_row(sec, r)
        )
        self._update_part_hint(section_row, prow)

    def _mark_section_custom(self, section_row: SectionRow) -> None:
        section_row.legacy_flat = False

    def _remove_part_row(self, section_row: SectionRow, row: PartRow) -> None:
        if row in section_row.parts:
            section_row.parts.remove(row)
            section_row.parts_layout.removeWidget(row.box)
            row.box.deleteLater()

    def _move_part(self, section_row: SectionRow, row: PartRow, delta: int) -> None:
        if row not in section_row.parts:
            return
        idx = section_row.parts.index(row)
        new = idx + delta
        if new < 0 or new >= len(section_row.parts):
            return
        section_row.parts[idx], section_row.parts[new] = (
            section_row.parts[new],
            section_row.parts[idx],
        )
        section_row.parts_layout.removeWidget(row.box)
        section_row.parts_layout.insertWidget(new, row.box)

    def _update_part_hint(self, section_row: SectionRow, row: PartRow) -> None:
        exam_id = str(section_row.bank.currentData() or "")
        bank = self.state.banks.get(exam_id)
        if bank is None:
            row.hint.setText("请选择题库")
            row.hint.setStyleSheet("color:#c04040;font-size:11px;")
            return
        row.count.setMaximum(max(1, len(bank.questions)))
        available = sum(1 for q in bank.questions if q.difficulty == int(row.difficulty.value()))
        if available < int(row.count.value()):
            row.hint.setText(f"⚠ 难度 {row.difficulty.value()} 只有 {available} 题")
            row.hint.setStyleSheet("color:#c04040;font-size:11px;")
        else:
            row.hint.setText(f"可用 {available} 题")
            row.hint.setStyleSheet("color:#888;font-size:11px;")

    def _refresh_part_hints(self, section_row: SectionRow) -> None:
        for part in section_row.parts:
            self._update_part_hint(section_row, part)

    def _remove_section_row(self, row: SectionRow) -> None:
        if row in self.section_rows:
            self.section_rows.remove(row)
            self.sections_layout.removeWidget(row.box)
            row.box.deleteLater()

    def _move_section(self, row: SectionRow, delta: int) -> None:
        if row not in self.section_rows:
            return
        idx = self.section_rows.index(row)
        new = idx + delta
        if new < 0 or new >= len(self.section_rows):
            return
        self.section_rows[idx], self.section_rows[new] = (
            self.section_rows[new],
            self.section_rows[idx],
        )
        self.sections_layout.removeWidget(row.box)
        self.sections_layout.insertWidget(new, row.box)

    def _refresh_section_banks(self) -> None:
        for row in self.section_rows:
            cur = row.bank.currentData()
            row.bank.blockSignals(True)
            row.bank.clear()
            for exam_id, b in sorted(self.state.banks.items()):
                row.bank.addItem(b.title, exam_id)
            idx = row.bank.findData(cur)
            if idx < 0 and cur:
                row.bank.addItem("⚠ 原题库已删除，请重新选择", "")
                idx = row.bank.count() - 1
            row.bank.setCurrentIndex(max(0, idx))
            row.bank.blockSignals(False)
            self._refresh_part_hints(row)

    def _collect_sections(self) -> list[Section] | None:
        out: list[Section] = []
        seen_sections: set[str] = set()
        seen_titles: set[str] = set()
        per_key: dict[tuple[str, int], int] = {}
        per_bank: dict[str, int] = {}
        for srow in self.section_rows:
            title = srow.title.text().strip()
            exam_id = str(srow.bank.currentData() or "")
            bank = self.state.banks.get(exam_id)
            if not title or bank is None:
                QMessageBox.warning(self, "大题不完整", "每个大题都要有标题并选择题库。")
                return None
            if title in seen_titles:
                QMessageBox.warning(self, "大题标题重复", f"「{title}」出现了两次。")
                return None
            seen_titles.add(title)
            if srow.section_id in seen_sections:
                QMessageBox.warning(self, "大题编号重复", srow.section_id)
                return None
            seen_sections.add(srow.section_id)
            if not srow.parts:
                QMessageBox.warning(self, "大题没有部分", f"「{title}」至少需要一个部分。")
                return None
            parts: list[SectionPart] = []
            part_titles: set[str] = set()
            part_ids: set[str] = set()
            for prow in srow.parts:
                ptitle = prow.title.text().strip()
                if not ptitle:
                    QMessageBox.warning(self, "部分不完整", f"「{title}」有部分没有标题。")
                    return None
                if ptitle in part_titles:
                    QMessageBox.warning(self, "部分标题重复", f"「{title} / {ptitle}」重复。")
                    return None
                part_titles.add(ptitle)
                if prow.part_id in part_ids:
                    QMessageBox.warning(
                        self,
                        "部分编号重复",
                        f"「{title}」部分编号 {prow.part_id} 重复。",
                    )
                    return None
                part_ids.add(prow.part_id)
                count = int(prow.count.value())
                diff = int(prow.difficulty.value())
                available = sum(1 for q in bank.questions if q.difficulty == diff)
                key = (exam_id, diff)
                per_key[key] = per_key.get(key, 0) + count
                per_bank[exam_id] = per_bank.get(exam_id, 0) + count
                if per_key[key] > available:
                    QMessageBox.warning(
                        self,
                        "题数不足",
                        f"题库「{bank.title}」难度 {diff} 多个部分共需 "
                        f"{per_key[key]} 题，可用 {available} 题。",
                    )
                    return None
                if per_bank[exam_id] > len(bank.questions):
                    QMessageBox.warning(
                        self,
                        "题数不足",
                        f"题库「{bank.title}」多个大题/部分共需 {per_bank[exam_id]} 题，"
                        f"但题库只有 {len(bank.questions)} 题。",
                    )
                    return None
                parts.append(
                    SectionPart(
                        part_id=prow.part_id,
                        title=ptitle,
                        count=count,
                        difficulty=diff,
                    )
                )
            total = sum(p.count for p in parts)
            if srow.legacy_flat and len(parts) == 1:
                # 未改动的旧单层工程：继续写 parts=[]，保持 v6 兼容
                out.append(
                    Section(
                        section_id=srow.section_id,
                        title=title,
                        file=f"{exam_id}.json",
                        count=total,
                        difficulty=parts[0].difficulty,
                        parts=[],
                    )
                )
                continue
            if srow.was_legacy and not srow.legacy_flat:
                answer = QMessageBox.question(
                    self,
                    "升级为两层大题",
                    "这个场次原本是单层大题，保存后会升级为 v7；\n"
                    "v0.14.x 及更早的学生端将无法打开新包。继续吗？",
                    QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
                )
                if answer != QMessageBox.StandardButton.Yes:
                    return None
            out.append(
                Section(
                    section_id=srow.section_id,
                    title=title,
                    file=f"{exam_id}.json",
                    count=total,
                    difficulty=parts[0].difficulty,
                    parts=parts,
                )
            )
        return out

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
        prev: dict[str, tuple[bool, int, bool, int, int]] = {
            r.exam_id: (
                r.cb.isChecked(),
                int(r.pick.value()),
                r.limit.isChecked(),
                int(r.dmin.value()),
                int(r.dmax.value()),
            )
            for r in self.checks
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
            # 外部题库导入时 title 默认等于 exam_id，此时括号里再写一遍就是重复
            label = (
                f"{bank.title}（题量 {len(bank.questions)}）"
                if bank.title == exam_id
                else f"{bank.title}（{exam_id}，题量 {len(bank.questions)}）"
            )
            cb = QCheckBox(label)
            # 整数用 QSpinBox：QDoubleSpinBox 即便 decimals=0 也会为
            # 小数点预留编辑区，把 "500" 截成 "50"。
            pick = QSpinBox()
            pick.setRange(1, max(len(bank.questions), 1))
            pick.setValue(min(bank.pick, len(bank.questions)))
            pick.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
            pick.setMinimumWidth(pick_width(int(pick.maximum())))
            limit = QCheckBox("限制平均难度")
            limit.setToolTip("勾选后，抽出的这 N 题平均难度必须落在下面区间（1–255）")
            dmin = QSpinBox()
            dmin.setRange(1, 255)
            dmin.setValue(1)
            dmax = QSpinBox()
            dmax.setRange(1, 255)
            dmax.setValue(5)
            for w in (dmin, dmax):
                w.setEnabled(False)
                w.setMinimumWidth(56)
            limit.toggled.connect(dmin.setEnabled)
            limit.toggled.connect(dmax.setEnabled)

            hl.addWidget(cb)
            hl.addWidget(QLabel("抽题数"))
            hl.addWidget(pick)
            hl.addSpacing(8)
            hint = QLabel("")
            hint.setStyleSheet("color: #888; font-size: 11px;")
            hl.addWidget(limit)
            hl.addWidget(dmin)
            hl.addWidget(QLabel("–"))
            hl.addWidget(dmax)
            hl.addWidget(hint)
            hl.addStretch(1)
            self.combo_holder.addWidget(row)
            br = BankRow(cb, pick, limit, dmin, dmax, hint, exam_id)
            self.checks.append(br)
            pick.valueChanged.connect(lambda _v, r=br: self._update_range_hint(r))
            self._update_range_hint(br)

        # 恢复快照；新增的题库没有快照，保持默认（未勾选）。
        for r in self.checks:
            if r.exam_id in prev:
                checked, n, limited, lo, hi = prev[r.exam_id]
                r.cb.setChecked(checked)
                r.pick.setValue(min(n, int(r.pick.maximum())))
                r.limit.setChecked(limited)
                r.dmin.setValue(lo)
                r.dmax.setValue(hi)

        self._refresh_section_banks()
        self.refresh()
        self._sync_roster()

    def _update_range_hint(self, r: BankRow) -> None:
        """显示"该库抽 N 题的可达平均分范围"，并提示未标注难度的题数。"""
        bank = self.state.banks.get(r.exam_id)
        if bank is None:
            r.hint.setText("")
            return
        missing = sum(1 for q in bank.questions if not q.difficulty)
        if missing:
            r.hint.setText(f"⚠ {missing} 题未标难度")
            r.hint.setStyleSheet("color: #c04040; font-size: 11px;")
            return
        diffs = sorted(int(q.difficulty) for q in bank.questions if q.difficulty)
        k = int(r.pick.value())
        if not diffs or k < 1 or k > len(diffs):
            r.hint.setText("")
            return
        lo = sum(diffs[:k]) / k
        hi = sum(diffs[-k:]) / k
        r.hint.setText(f"可达 {lo:.1f}–{hi:.1f}")
        r.hint.setStyleSheet("color: #888; font-size: 11px;")

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
            self.combo_roster.setCurrentIndex(self.combo_roster.findData("all"))
            # 索引可能本来就是 all（不会触发信号），必须显式重填当前全局名单
            self.edit_roster.setText(" ".join(str(x) for x in self.state.students))
        else:
            self.combo_roster.setCurrentIndex(self.combo_roster.findData("custom"))
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
        for r in self.checks:
            e = resolved.get(r.exam_id)
            r.cb.setChecked(e is not None)
            if e is not None:
                if e.pick:
                    r.pick.setValue(e.pick)
                r.limit.setChecked(e.difficulty_min is not None)
                if e.difficulty_min is not None:
                    r.dmin.setValue(e.difficulty_min)
                    r.dmax.setValue(e.difficulty_max or e.difficulty_min)
                else:
                    # 没有区间时重置为默认，避免沿用上一场的值
                    r.dmin.setValue(1)
                    r.dmax.setValue(5)

        # 大题模式
        for row in list(self.section_rows):
            self._remove_section_row(row)
        for sec in s.sections:
            self._add_section_row(sec)
        self.chk_sections.blockSignals(True)
        self.chk_sections.setChecked(bool(s.sections))
        self.chk_sections.blockSignals(False)
        self._on_sections_toggle(bool(s.sections))

    def _sync_roster(self) -> None:
        """学生名单变化后，把「使用全部学生」的显示刷新成最新名单。"""
        if self.combo_roster.currentData() == "all":
            self.edit_roster.setText(" ".join(str(x) for x in self.state.students))
            self._update_roster_info()

    def _on_roster_mode(self) -> None:
        mode = self.combo_roster.currentData()
        if mode == "by_class":
            picked = self._pick_classes()
            if not picked:
                # 取消或一个班都没勾：**不能**把已有名单清空（实测会静默丢名单）
                if picked is not None:
                    QMessageBox.information(
                        self, "没有选择班级", "没有勾选任何班级，本场名单保持不变。"
                    )
                prev = getattr(self, "_roster_mode", "custom")
                self.combo_roster.blockSignals(True)
                self.combo_roster.setCurrentIndex(self.combo_roster.findData(prev))
                self.combo_roster.blockSignals(False)
                self.edit_roster.setEnabled(prev == "custom")
                self._update_roster_info()
                return
            # 选完就切到"自定义"：场次里存的永远是具体学号（快照），
            # 不依赖班级后续怎么改。
            self.combo_roster.blockSignals(True)
            self.combo_roster.setCurrentIndex(self.combo_roster.findData("custom"))
            self.combo_roster.blockSignals(False)
            mode = "custom"
            merged = list(dict.fromkeys([*picked]))
            self.edit_roster.setText(" ".join(str(x) for x in merged))
        self._roster_mode = mode
        self.edit_roster.setEnabled(mode == "custom")
        if mode == "all":
            # 必须**无条件**重填：文本框里可能还留着上一场的旧名单，
            # 只在为空时填会显示过期名单（客户实测：加完学生这里不变）。
            self.edit_roster.setText(" ".join(str(x) for x in self.state.students))
        self._update_roster_info()

    def _update_roster_info(self) -> None:
        """显示本场人数与来源班级（用于自查漏加）。"""
        ids = []
        for tok in self.edit_roster.text().replace("，", ",").replace(",", " ").split():
            if tok.isdigit():
                ids.append(int(tok))
        ids = list(dict.fromkeys(ids))
        if not ids:
            self.lbl_roster_info.setText("")
            return
        sources: dict[str, int] = {}
        for sid in ids:
            for name in self.state.classes_of(sid):
                sources[name] = sources.get(name, 0) + 1
        if sources:
            detail = "；".join(f"{n}：{c} 人" for n, c in sorted(sources.items()))
            self.lbl_roster_info.setText(
                f"本场 {len(ids)} 人，来自 {len(sources)} 个班（{detail}）"
            )
        else:
            self.lbl_roster_info.setText(f"本场 {len(ids)} 人（未匹配到班级）")

    def _pick_classes(self) -> list[int] | None:
        """按班级选人：多选班级，返回去重后的学号；取消返回 None。"""
        if not self.state.classes:
            QMessageBox.information(self, "还没有班级", "请先到「学生」页新建班级并导入成员。")
            return None
        dlg = QDialog(self)
        dlg.setWindowTitle("按班级添加学生")
        lay = QVBoxLayout(dlg)
        lay.addWidget(QLabel("勾选要加入本场的班级（可多选）："))
        lst = QListWidget(dlg)
        for c in self.state.classes:
            item = QListWidgetItem(
                f"{c.name}（{len(c.students)} 人）" + (f"  [{c.code}]" if c.code else "")
            )
            item.setFlags(item.flags() | Qt.ItemFlag.ItemIsUserCheckable)
            item.setCheckState(Qt.CheckState.Unchecked)
            item.setData(Qt.ItemDataRole.UserRole, c.name)
            lst.addItem(item)
        lay.addWidget(lst, 1)
        btns = QDialogButtonBox(
            QDialogButtonBox.StandardButton.Ok | QDialogButtonBox.StandardButton.Cancel
        )
        btns.accepted.connect(dlg.accept)
        btns.rejected.connect(dlg.reject)
        lay.addWidget(btns)
        if dlg.exec() != QDialog.DialogCode.Accepted:
            return None
        picked: list[int] = []
        for i in range(lst.count()):
            item = lst.item(i)
            if item.checkState() == Qt.CheckState.Checked:
                c = self.state.class_by_name(item.data(Qt.ItemDataRole.UserRole))
                if c is not None:
                    picked.extend(c.students)
        return picked

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

        # 科目 / 大题
        examples: list[SessionExam] = []
        sections: list[Section] = []
        if self.chk_sections.isChecked():
            collected = self._collect_sections()
            if collected is None:
                return
            if not collected:
                QMessageBox.warning(self, "没有大题", "按大题模式至少需要一个大题。")
                return
            sections = collected
        else:
            for r in self.checks:
                if not r.cb.isChecked():
                    continue
                lo = hi = None
                if r.limit.isChecked():
                    lo, hi = int(r.dmin.value()), int(r.dmax.value())
                    bank = self.state.banks.get(r.exam_id)
                    if bank is None:
                        QMessageBox.critical(self, "题库缺失", f"题库 {r.exam_id} 不在当前工程里。")
                        return
                    problems = check_difficulty_range(bank, int(r.pick.value()), lo, hi)
                    if problems:
                        QMessageBox.critical(self, "平均难度不可行", "\n".join(problems))
                        return
                examples.append(
                    SessionExam(
                        file=f"{r.exam_id}.json",
                        pick=int(r.pick.value()),
                        difficulty_min=lo,
                        difficulty_max=hi,
                    )
                )

        old_id = self._current.session_id if self._current is not None else None
        try:
            s = Session(
                session_id=int(self.spin_id.value()),
                start=local_to_rfc3339(self.dt_start.dateTime().toString("yyyy-MM-dd HH:mm")),
                end=local_to_rfc3339(self.dt_end.dateTime().toString("yyyy-MM-dd HH:mm")),
                students=students,
                exams=examples,
                sections=sections,
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
            # 编辑时若改了编号，先删旧编号，否则会新增一场并留下旧场次。
            if old_id is not None and old_id != s.session_id:
                self.state.remove_session(old_id)
            self.state.upsert_session(s)
        self.refresh()
        self._load_into_form(s)
        self.changed.emit()
        QMessageBox.information(self, "已保存", f"场次 #{s.session_id} 已保存。")
