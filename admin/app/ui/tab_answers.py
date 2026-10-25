"""答题卡 Tab：批量解密、校验学生交上来的 .eea 文件。

数据来源是学生端在学生机器上写的加密答题卡。解密需要主密钥，所以这一页
要求教师粘贴主密钥——与打包页相同的红线：**只经环境变量传给子进程**，
不落盘、不出现在进程列表里。
"""

from __future__ import annotations

import csv
import json
import os
import subprocess
from datetime import datetime
from pathlib import Path

from PySide6.QtCore import Qt, QThread, Signal
from PySide6.QtWidgets import (
    QAbstractItemView,
    QCheckBox,
    QFileDialog,
    QGroupBox,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QLineEdit,
    QMessageBox,
    QPushButton,
    QTableWidget,
    QTableWidgetItem,
    QVBoxLayout,
    QWidget,
)

from app.core.grading import TYPE_LABELS, Policy, grade_sheet, summarize
from app.core.state import ProjectState
from app.ui.tab_build import builder_exe

COLUMNS = ["文件", "学号", "场次", "题数", "已答", "未答", "交卷时间", "状态", "说明"]

GRADE_COLUMNS = [
    "学号",
    "场次",
    "题号",
    "题型",
    "状态",
    "学生作答",
    "标准答案",
    "得分",
    "满分",
    "人工调整",
]


def collect_eea(paths: list[Path]) -> list[Path]:
    """展开拖进来的路径：目录取其下所有 .eea，文件只收 .eea。"""
    out: list[Path] = []
    for p in paths:
        if p.is_dir():
            out.extend(sorted(p.glob("*.eea")))
        elif p.suffix.lower() == ".eea":
            out.append(p)
    return out


def run_tool(cmd: list[str], env: dict[str, str]) -> tuple[str, str, int]:
    """调用外部工具并按 **UTF-8** 解码输出。

    为什么必须单独抽出来、且显式指定 encoding：
    `subprocess.run(..., text=True)` 在中文 Windows 上会用 GBK 解码子进程输出，
    而 `exam-builder` 输出的是 UTF-8。解码发生在读取线程里，失败时异常不会抛给
    调用方，只会得到空字符串——界面上表现为"无输出"，把真实原因彻底吞掉
    （v0.3.1 实测：教师输入正确 master 也解不开，实际是解码失败）。
    """
    cp = subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        env=env,
        timeout=120,
    )
    return cp.stdout or "", cp.stderr or "", cp.returncode


class VerifyWorker(QThread):
    """逐个调用 `exam-builder verify-answer-sheet --json`。"""

    one_done = Signal(str, dict)
    all_done = Signal()

    def __init__(self, files: list[Path], master: str, package: Path | None) -> None:
        super().__init__()
        self.files = files
        self.master = master
        self.package = package

    def run(self) -> None:  # noqa: D102 - QThread 约定
        for f in self.files:
            # 用完整路径而不是文件名：不同机器会产出同名文件
            # （同一学号同一场次），按名字索引会互相覆盖。
            self.one_done.emit(str(f), self._verify_one(f))
        self.all_done.emit()

    def _verify_one(self, path: Path) -> dict:
        cmd = [str(builder_exe()), "verify-answer-sheet", str(path), "--json"]
        if self.package is not None:
            cmd += ["--package", str(self.package)]
        env = dict(os.environ)
        env["EXAM_MASTER"] = self.master
        try:
            out, err, code = run_tool(cmd, env)
        except Exception as e:  # noqa: BLE001 - 子进程任何异常都要变成一行结果
            return {"ok": False, "message": f"调用校验工具失败：{e}"}
        line = out.strip().splitlines()
        payload = {}
        if line:
            try:
                payload = json.loads(line[-1])
            except json.JSONDecodeError:
                payload = {"ok": False, "message": (out or err).strip()[:200]}
        if not payload:
            detail = (err or out or "").strip()
            if not detail:
                detail = (
                    f"校验工具没有任何输出（退出码 {code}）——可能被杀毒软件拦截，或程序文件损坏"
                )
            payload = {"ok": False, "message": detail[:200]}
        return payload


class AnswersTab(QWidget):
    """答题卡列表 + 校验 + 自动判分 + 导出。"""

    def __init__(self, state: ProjectState, parent: QWidget | None = None) -> None:
        super().__init__(parent)
        self.state = state
        self.files: list[Path] = []
        self.results: dict[str, dict] = {}
        self.worker: VerifyWorker | None = None
        #: 判分明细（ItemResult），跨学生展平
        self.grade_rows: list = []

        lay = QVBoxLayout(self)
        self.setAcceptDrops(True)

        tip = QLabel(
            "把学生交上来的 answers-<学号>-<场次>-<指纹8位>.eea 拖进来（或点【添加文件】）。\n"
            "校验需要主密钥；指定学生端包后还会核对「这份答题卡是否属于这个包的这个学生」。"
        )
        tip.setWordWrap(True)
        lay.addWidget(tip)

        row = QHBoxLayout()
        row.addWidget(QLabel("主密钥"))
        self.master = QLineEdit()
        self.master.setEchoMode(QLineEdit.EchoMode.Password)
        self.master.setPlaceholderText("64 位十六进制；只经环境变量传给校验进程")
        row.addWidget(self.master, 1)
        self.btn_eye = QPushButton("显示")
        self.btn_eye.setCheckable(True)
        self.btn_eye.setFixedWidth(56)
        self.btn_eye.toggled.connect(
            lambda on: self.master.setEchoMode(
                QLineEdit.EchoMode.Normal if on else QLineEdit.EchoMode.Password
            )
        )
        row.addWidget(self.btn_eye)

        row.addWidget(QLabel("学生端包（可选）"))
        self.pkg = QLineEdit()
        self.pkg.setPlaceholderText("用于绑定校验")
        row.addWidget(self.pkg, 1)
        btn_pkg = QPushButton("选择…")
        btn_pkg.clicked.connect(self._pick_package)
        row.addWidget(btn_pkg)
        lay.addLayout(row)

        btns = QHBoxLayout()
        self.btn_add = QPushButton("添加文件…")
        self.btn_add.clicked.connect(self._add_files)
        btns.addWidget(self.btn_add)
        self.btn_dir = QPushButton("添加文件夹…")
        self.btn_dir.clicked.connect(self._add_dir)
        btns.addWidget(self.btn_dir)
        self.btn_clear = QPushButton("清空列表")
        self.btn_clear.clicked.connect(self._clear)
        btns.addWidget(self.btn_clear)
        btns.addStretch(1)
        self.btn_verify = QPushButton("开始校验")
        self.btn_verify.clicked.connect(self._start)
        btns.addWidget(self.btn_verify)
        self.btn_csv = QPushButton("导出原始作答 CSV")
        self.btn_csv.setToolTip("只导出学生原始作答，不含分数；判分请用下方「导出判分结果…」")
        self.btn_csv.clicked.connect(self._export_csv)
        self.btn_csv.setEnabled(False)
        btns.addWidget(self.btn_csv)
        lay.addLayout(btns)

        self.table = QTableWidget(0, len(COLUMNS))
        self.table.setHorizontalHeaderLabels(COLUMNS)
        self.table.setEditTriggers(QAbstractItemView.EditTrigger.NoEditTriggers)
        self.table.setSelectionBehavior(QAbstractItemView.SelectionBehavior.SelectRows)
        header = self.table.horizontalHeader()
        header.setSectionResizeMode(QHeaderView.ResizeMode.ResizeToContents)
        header.setStretchLastSection(True)
        lay.addWidget(self.table, 1)

        self.status = QLabel("尚未校验")
        lay.addWidget(self.status)

        # ---------------------------------------------------------------- 判分
        box = QGroupBox("自动判分")
        bl = QVBoxLayout(box)

        type_row = QHBoxLayout()
        type_row.addWidget(QLabel("自动判分题型："))
        self.chk_types: dict[str, QCheckBox] = {}
        for key, label in (
            ("single", "单选题"),
            ("multiple", "多选题"),
            ("fill", "填空题"),
            ("fix", "改错题"),
        ):
            cb = QCheckBox(label)
            cb.setChecked(key in ("single", "multiple"))
            self.chk_types[key] = cb
            type_row.addWidget(cb)
        type_row.addStretch(1)
        bl.addLayout(type_row)

        rule_row = QHBoxLayout()
        self.chk_case = QCheckBox("忽略大小写")
        self.chk_case.setChecked(True)
        self.chk_space = QCheckBox("忽略空白字符")
        self.chk_space.setChecked(True)
        self.chk_partial = QCheckBox("填空/改错按项给分")
        self.chk_partial.setToolTip("默认整题全对才给分；勾上后按答对的空/行比例给分")
        rule_row.addWidget(QLabel("匹配规则："))
        rule_row.addWidget(self.chk_case)
        rule_row.addWidget(self.chk_space)
        rule_row.addWidget(self.chk_partial)
        rule_row.addStretch(1)
        bl.addLayout(rule_row)

        gbtn = QHBoxLayout()
        self.btn_precheck = QPushButton("预检")
        self.btn_precheck.clicked.connect(self._precheck)
        self.btn_grade = QPushButton("生成成绩")
        self.btn_grade.clicked.connect(self._grade)
        self.btn_grade_csv = QPushButton("导出判分结果…")
        self.btn_grade_csv.clicked.connect(self._export_grading)
        self.btn_grade_csv.setEnabled(False)
        gbtn.addWidget(self.btn_precheck)
        gbtn.addWidget(self.btn_grade)
        gbtn.addWidget(self.btn_grade_csv)
        gbtn.addStretch(1)
        bl.addLayout(gbtn)

        self.grade_table = QTableWidget(0, len(GRADE_COLUMNS))
        self.grade_table.setHorizontalHeaderLabels(GRADE_COLUMNS)
        self.grade_table.setEditTriggers(
            QAbstractItemView.EditTrigger.DoubleClicked
            | QAbstractItemView.EditTrigger.SelectedClicked
            | QAbstractItemView.EditTrigger.EditKeyPressed
        )
        self.grade_table.setSelectionBehavior(QTableWidget.SelectionBehavior.SelectRows)
        gh = self.grade_table.horizontalHeader()
        gh.setSectionResizeMode(QHeaderView.ResizeMode.ResizeToContents)
        gh.setStretchLastSection(True)
        self.grade_table.itemChanged.connect(self._on_score_edited)
        bl.addWidget(self.grade_table, 1)

        self.grade_summary = QLabel("尚未判分")
        bl.addWidget(self.grade_summary)
        lay.addWidget(box, 1)

        # 结果只对"当时那把主密钥 + 那个包"有效；改了输入还留着"通过"等于给错误背书
        self.master.textChanged.connect(self._invalidate)
        self.pkg.textChanged.connect(self._invalidate)

    # ---------------------------------------------------------------- 拖放

    def dragEnterEvent(self, event) -> None:  # noqa: N802 - Qt 约定
        if self._busy():
            event.ignore()
            return
        if event.mimeData().hasUrls() and any(
            u.toLocalFile().lower().endswith(".eea") for u in event.mimeData().urls()
        ):
            event.acceptProposedAction()

    def dropEvent(self, event) -> None:  # noqa: N802 - Qt 约定
        if self._busy():
            event.ignore()
            return
        files = collect_eea([Path(u.toLocalFile()) for u in event.mimeData().urls()])
        if files:
            self._append(files)
            event.acceptProposedAction()

    def _reset_grading(self) -> None:
        """输入变了，旧成绩必须一起作废——否则会导出与当前输入不符的分数。"""
        self.grade_rows = []
        self.grade_table.blockSignals(True)
        self.grade_table.setRowCount(0)
        self.grade_table.blockSignals(False)
        self.grade_summary.setText("尚未判分")
        self.btn_grade_csv.setEnabled(False)

    def _invalidate(self) -> None:
        """主密钥/包变了，之前的结果全部作废。"""
        if self._busy():
            # 校验进行中输入本该是禁用的；这里是双保险，防止内部改动绕过
            return
        self._reset_grading()
        if not self.results:
            return
        self.results.clear()
        self.btn_csv.setEnabled(False)
        self._refill()
        self.status.setText("输入已变更，请重新校验")

    # ---------------------------------------------------------------- 文件

    def _pick_package(self) -> None:
        path, _ = QFileDialog.getOpenFileName(
            self, "选择学生端包", "", "可执行文件 (*.exe);;所有文件 (*)"
        )
        if path:
            self.pkg.setText(path)

    def _add_files(self) -> None:
        paths, _ = QFileDialog.getOpenFileNames(
            self, "选择答题卡", "", "答题卡 (*.eea);;所有文件 (*)"
        )
        self._append([Path(p) for p in paths])

    def _add_dir(self) -> None:
        d = QFileDialog.getExistingDirectory(self, "选择包含答题卡的文件夹")
        if d:
            self._append(sorted(Path(d).glob("*.eea")))

    def _busy(self) -> bool:
        """校验进行中：列表必须冻结，否则结果会和表格错位。"""
        return self.worker is not None and self.worker.isRunning()

    def _append(self, paths: list[Path]) -> None:
        if self._busy():
            return
        for p in paths:
            if p not in self.files:
                self.files.append(p)
        # 列表变了，上一次的校验结果不再可信——清掉，避免导出时混进旧文件
        self.results.clear()
        self.btn_csv.setEnabled(False)
        self._refill()

    def _clear(self) -> None:
        if self._busy():
            return
        self.files.clear()
        self.results.clear()
        self._reset_grading()
        self._refill()
        self.btn_csv.setEnabled(False)
        self.status.setText("尚未校验")

    def _refill(self) -> None:
        self.table.setRowCount(len(self.files))
        for row, path in enumerate(self.files):
            self.table.setItem(row, 0, QTableWidgetItem(path.name))
            for col in range(1, len(COLUMNS)):
                self._set(row, col, "")
        self.status.setText(f"共 {len(self.files)} 个文件，尚未校验")

    def _set(self, row: int, col: int, text: str, color: str | None = None) -> None:
        item = QTableWidgetItem(text)
        if color:
            from PySide6.QtGui import QColor

            item.setForeground(QColor(color))
        self.table.setItem(row, col, item)

    # ---------------------------------------------------------------- 校验

    def _start(self) -> None:
        if self._busy():
            return
        self._reset_grading()
        if not self.files:
            QMessageBox.information(self, "提示", "请先添加答题卡文件。")
            return
        master = self.master.text().strip()
        if len(master) != 64:
            QMessageBox.warning(self, "主密钥不对", "主密钥应为 64 位十六进制。")
            return
        pkg_text = self.pkg.text().strip()
        pkg = Path(pkg_text) if pkg_text else None
        if pkg is not None and not pkg.is_file():
            QMessageBox.warning(self, "找不到包", f"学生端包不存在：{pkg}")
            return

        self.results.clear()
        self._set_editing_enabled(False)
        self.btn_verify.setEnabled(False)
        self.status.setText("校验中…")
        self.worker = VerifyWorker(self.files, master, pkg)
        self.worker.one_done.connect(self._on_one)
        self.worker.all_done.connect(self._on_all)
        self.worker.start()

    def _on_one(self, key: str, result: dict) -> None:
        self.results[key] = result
        row = next((i for i, p in enumerate(self.files) if str(p) == key), None)
        if row is None:
            return
        sheet = result.get("sheet") or {}
        ok = bool(result.get("ok"))
        binding = result.get("binding")
        if ok:
            # 没给包时 binding 为 null：不能显示成"通过"就完事，教师要知道没校验绑定
            status = "✓ 通过（已绑定）" if binding else "✓ 通过（未校验绑定）"
        else:
            status = "✗ 失败"
        if not sheet:
            for col in range(1, 7):
                self._set(row, col, "—")
        else:
            total = len(sheet.get("qids") or [])
            answered = sum(1 for a in (sheet.get("answers") or []) if a is not None)
            self._set(row, 1, str(sheet.get("student_id", "—")))
            self._set(row, 2, str(sheet.get("session_id", "—")))
            self._set(row, 3, str(total))
            self._set(row, 4, str(answered))
            self._set(row, 5, str(total - answered))
            sub = sheet.get("submitted_unix")
            submitted = datetime.fromtimestamp(sub).strftime("%Y-%m-%d %H:%M") if sub else "未交卷"
            self._set(row, 6, submitted)
        self._set(row, 7, status, None if ok else "#c0392b")
        self._set(row, 8, "" if ok else str(result.get("message", ""))[:120])
        tooltip = str(result.get("message", ""))
        if tooltip:
            self.table.item(row, 7).setToolTip(tooltip)

    def _set_editing_enabled(self, on: bool) -> None:
        # 主密钥/包也算"编辑态"：校验中改它们会触发 _invalidate，把结果清空
        for w in (
            self.btn_add,
            self.btn_dir,
            self.btn_clear,
            self.master,
            self.pkg,
            self.btn_eye,
        ):
            w.setEnabled(on)

    def _on_all(self) -> None:
        self._set_editing_enabled(True)
        self.btn_verify.setEnabled(True)
        ok = sum(1 for r in self.results.values() if r.get("ok"))
        self.status.setText(f"校验完成：{ok} 通过 / {len(self.results) - ok} 失败")
        self.btn_csv.setEnabled(ok > 0)

    # ---------------------------------------------------------------- 导出

    def _export_csv(self) -> None:
        rows = []
        for path in self.files:
            name = path.name
            result = self.results.get(str(path)) or {}
            sheet = result.get("sheet") or {}
            if not result.get("ok"):
                continue
            sub = sheet.get("submitted_unix")
            submitted = datetime.fromtimestamp(sub).strftime("%Y-%m-%d %H:%M:%S") if sub else ""
            for idx, qid in enumerate(sheet.get("qids") or []):
                answers = sheet.get("answers") or []
                value = answers[idx] if idx < len(answers) else None
                rows.append(
                    {
                        "文件": name,
                        "学号": sheet.get("student_id", ""),
                        "场次": sheet.get("session_id", ""),
                        "题号": idx + 1,
                        "题目ID": qid,
                        "作答": "" if value is None else value,
                        "是否作答": "否" if value is None else "是",
                        "交卷时间": submitted,
                    }
                )
        if not rows:
            QMessageBox.information(self, "提示", "没有可导出的通过项。")
            return
        path, _ = QFileDialog.getSaveFileName(self, "导出 CSV", "答题卡明细.csv", "CSV (*.csv)")
        if not path:
            return
        # utf-8-sig：Excel 直接双击打开不会乱码
        with open(path, "w", encoding="utf-8-sig", newline="") as f:
            writer = csv.DictWriter(f, fieldnames=list(rows[0].keys()))
            writer.writeheader()
            writer.writerows(rows)
        QMessageBox.information(self, "完成", f"已导出 {len(rows)} 行到\n{path}")

    # ---------------------------------------------------------------- 判分

    def _policy(self) -> Policy:
        enabled = frozenset(k for k, cb in self.chk_types.items() if cb.isChecked())
        return Policy(
            enabled=enabled,
            ignore_case=self.chk_case.isChecked(),
            ignore_space=self.chk_space.isChecked(),
            blank_partial=self.chk_partial.isChecked(),
        )

    def _bank_by_qid(self) -> dict:
        out = {}
        for bank in self.state.banks.values():
            for q in bank.questions:
                out[q.id] = q
        return out

    def _verified_sheets(self) -> list[dict]:
        sheets = []
        for path in self.files:
            result = self.results.get(str(path)) or {}
            if result.get("ok"):
                sheets.append(result.get("sheet") or {})
        return sheets

    def _precheck(self) -> None:
        """判分前先把"数据不全"的情况列出来，避免判到一半才发现。"""
        sheets = self._verified_sheets()
        if not sheets:
            QMessageBox.information(self, "提示", "请先完成校验。")
            return
        bank = self._bank_by_qid()
        if not bank:
            QMessageBox.warning(self, "题库为空", "当前工程没有题库，无法判分。请先打开原工程。")
            return

        missing_qids: set[str] = set()
        missing_answer: set[str] = set()
        unsubmitted = 0
        unbound = 0
        for path in self.files:
            res = self.results.get(str(path)) or {}
            if res.get("ok") and res.get("binding") is False:
                unbound += 1
        for sheet in sheets:
            if not sheet.get("submitted"):
                unsubmitted += 1
            for qid in sheet.get("qids") or []:
                q = bank.get(str(qid))
                if q is None:
                    missing_qids.add(str(qid))
                elif q.answer in (None, {}, []):
                    missing_answer.add(str(qid))

        lines = [
            f"答题卡：{len(sheets)} 张",
            f"题库题目：{len(bank)} 道",
        ]
        if unsubmitted:
            lines.append(f"⚠ 未交卷：{unsubmitted} 张（仍可判分，汇总里会标出）")
        if unbound:
            lines.append(
                f"⚠ 与所选包不匹配：{unbound} 张"
                "（binding=false，可能来自旧题库/别的包，判分结果需人工复核）"
            )
        if missing_qids:
            lines.append(f"✗ 题库里找不到的题号：{len(missing_qids)} 个（换错工程了？）")
        if missing_answer:
            lines.append(f"⚠ 没有标准答案的题：{len(missing_answer)} 道（会标成未知）")
        if not missing_qids and not missing_answer:
            lines.append("✓ 数据齐全，可以判分")
        QMessageBox.information(self, "判分预检", "\n".join(lines))

    def _grade(self) -> None:
        sheets = self._verified_sheets()
        if not sheets:
            QMessageBox.information(self, "提示", "请先完成校验。")
            return
        bank = self._bank_by_qid()
        if not bank:
            QMessageBox.warning(self, "题库为空", "当前工程没有题库，无法判分。请先打开原工程。")
            return

        policy = self._policy()
        self.grade_rows = []
        for sheet in sheets:
            self.grade_rows.extend(grade_sheet(bank, sheet, policy))

        self._fill_grade_table()
        self.btn_grade_csv.setEnabled(bool(self.grade_rows))
        self.grade_summary.setText(self._summary_text(policy))

    def _fill_grade_table(self) -> None:
        self.grade_table.blockSignals(True)
        self.grade_table.setRowCount(0)
        for r in self.grade_rows:
            row = self.grade_table.rowCount()
            self.grade_table.insertRow(row)
            values = [
                str(r.student_id),
                str(r.session_id),
                str(r.seq),
                TYPE_LABELS.get(r.qtype, r.qtype or "-"),
                r.status,
                r.student,
                r.expected,
                str(r.final_score),
                str(r.max_score),
                f"{r.score} → {r.manual_score}" if r.adjusted else "",
            ]
            for col, text in enumerate(values):
                item = QTableWidgetItem(text)
                if col == 7:
                    item.setFlags(item.flags() | Qt.ItemFlag.ItemIsEditable)
                    item.setToolTip("双击可改分（0 ~ 满分）")
                else:
                    item.setFlags(item.flags() & ~Qt.ItemFlag.ItemIsEditable)
                self.grade_table.setItem(row, col, item)
        self.grade_table.blockSignals(False)

    def _on_score_edited(self, item: QTableWidgetItem) -> None:
        if item.column() != 7:
            return
        row = item.row()
        if row >= len(self.grade_rows):
            return
        result = self.grade_rows[row]
        try:
            value = int(item.text().strip())
        except ValueError:
            value = result.final_score
        value = max(0, min(value, result.max_score))
        result.manual_score = value if value != result.score else None
        result.adjusted_at = datetime.now().strftime("%Y-%m-%d %H:%M:%S") if result.adjusted else ""
        self.grade_table.blockSignals(True)
        item.setText(str(result.final_score))
        adjust = self.grade_table.item(row, 9)
        if adjust is not None:
            adjust.setText(f"{result.score} → {result.manual_score}" if result.adjusted else "")
        self.grade_table.blockSignals(False)
        self.grade_summary.setText(self._summary_text(self._policy()))

    def _summary_text(self, policy: Policy) -> str:
        if not self.grade_rows:
            return "尚未判分"
        s = summarize(self.grade_rows)
        unbound = 0
        for path in self.files:
            res = self.results.get(str(path)) or {}
            if res.get("ok") and res.get("binding") is False:
                unbound += 1
        tail = f"；⚠ {unbound} 张卡与所选包不匹配" if unbound else ""
        return (
            f"已判 {s['judged']} 题，得分 {s['score']} / {s['max_score']}；"
            f"未判 {s['unjudged']} 题{tail}。口径：{policy.describe()}"
        )

    def _export_grading(self) -> None:
        if not self.grade_rows:
            QMessageBox.information(self, "提示", "请先生成成绩。")
            return
        out_dir = QFileDialog.getExistingDirectory(self, "选择导出目录")
        if not out_dir:
            return
        policy = self._policy()
        base = Path(out_dir)
        stamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
        # 文件名带时间戳：同一目录导出第二场时不会静默覆盖上一场
        tag = datetime.now().strftime("%Y%m%d-%H%M%S")

        judged = [r for r in self.grade_rows if r.status not in ("unjudged", "unknown")]
        unjudged = [r for r in self.grade_rows if r.status in ("unjudged", "unknown")]

        def write(name: str, header: list[str], rows: list[list]) -> None:
            path = base / name
            with open(path, "w", encoding="utf-8-sig", newline="") as f:
                writer = csv.writer(f)
                writer.writerow(header)
                writer.writerows(rows)

        detail_header = [
            "学号",
            "场次",
            "题号",
            "题目ID",
            "题型",
            "状态",
            "学生作答",
            "标准答案",
            "得分",
            "满分",
            "人工调整",
            "调整时间",
        ]
        detail_rows = [
            [
                r.student_id,
                r.session_id,
                r.seq,
                r.qid,
                r.qtype,
                r.status,
                r.student,
                r.expected,
                r.final_score,
                r.max_score,
                f"{r.score} → {r.manual_score}" if r.adjusted else "",
                r.adjusted_at,
            ]
            for r in judged
        ]

        groups: dict[tuple[int, int], list] = {}
        for r in judged:
            groups.setdefault((r.student_id, r.session_id), []).append(r)
        summary_header = ["学号", "场次", "已判题数", "得分", "满分", "得分率"]
        summary_rows = []
        for (sid, sidx), rows in sorted(groups.items()):
            score = sum(x.final_score for x in rows)
            total = sum(x.max_score for x in rows)
            rate = f"{score / total:.1%}" if total else "-"
            summary_rows.append([sid, sidx, len(rows), score, total, rate])

        unjudged_header = ["学号", "场次", "题号", "题目ID", "题型", "学生作答", "满分", "状态"]
        unjudged_rows = [
            [r.student_id, r.session_id, r.seq, r.qid, r.qtype, r.student, r.max_score, r.status]
            for r in unjudged
        ]

        all_rows = [row + ["已判"] for row in detail_rows]
        all_rows += [
            [
                r.student_id,
                r.session_id,
                r.seq,
                r.qid,
                r.qtype,
                r.status,
                r.student,
                r.expected,
                r.final_score,
                r.max_score,
                "",
                "未判",
            ]
            for r in unjudged
        ]

        names = {
            "detail": f"已判分-明细-{tag}.csv",
            "summary": f"已判分-汇总-{tag}.csv",
            "unjudged": f"未判分-{tag}.csv",
            "all": f"全量-{tag}.csv",
        }
        write(names["detail"], detail_header, detail_rows)
        write(names["summary"], summary_header, summary_rows)
        write(names["unjudged"], unjudged_header, unjudged_rows)
        write(names["all"], detail_header + ["判分状态"], all_rows)

        (base / "说明.txt").write_text(
            "保密考试系统 判分结果\n\n"
            f"生成时间：{stamp}\n"
            f"判分口径：{policy.describe()}\n"
            f"已判题：{len(detail_rows)} 行；未判题：{len(unjudged_rows)} 行\n\n"
            "文件：\n"
            f"  {names['detail']}\n  {names['summary']}\n  {names['unjudged']}\n  {names['all']}\n",
            encoding="utf-8-sig",
        )

        QMessageBox.information(
            self,
            "导出完成",
            f"已写入 4 个 CSV + 说明.txt 到：\n{out_dir}\n\n"
            f"已判分明细 {len(detail_rows)} 行；未判分 {len(unjudged_rows)} 行。",
        )
