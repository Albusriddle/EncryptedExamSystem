"""打包 Tab：调用 exam-builder 生成学生端自解压包。

## 两个硬要求（来自代码审阅的教训）

1. **必须有实时进度**：1500 个信封约 7 秒，10 个场次 × 150 人；
   大规模时会到分钟级。窗口假死或只有转圈都会让教师以为程序崩了。
   用 `QThread` + `subprocess.Popen` 逐行读输出，日志实时滚动。

2. **master 用完立即清空**：无论成功失败都清，且绝不写进日志。
   `builder.py` 通过 `EXAM_MASTER` 环境变量传（不走命令行，避免进程列表泄漏）。
"""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from datetime import datetime
from pathlib import Path

from PySide6.QtCore import QThread, QTimer, QUrl, Signal
from PySide6.QtGui import QGuiApplication
from PySide6.QtWidgets import (
    QCheckBox,
    QComboBox,
    QFileDialog,
    QFormLayout,
    QGroupBox,
    QHBoxLayout,
    QInputDialog,
    QLabel,
    QLineEdit,
    QMessageBox,
    QPlainTextEdit,
    QProgressBar,
    QPushButton,
    QSpinBox,
    QVBoxLayout,
    QWidget,
)

from app.core.banks_io import write_temp_banks
from app.core.buildcmd import build_command, build_env, safe_pin_dir
from app.core.state import ProjectState
from app.core.subproc import no_window_kwargs
from app.ui.widgets import fit_button_text


def is_frozen() -> bool:
    """是否运行在 PyInstaller 打包后的环境里。"""
    return bool(getattr(sys, "frozen", False))


def program_dir() -> Path:
    """程序所在目录。

    冻结后是 ExamAdmin.exe 所在目录；源码运行时是仓库根。
    """
    return Path(sys.executable).parent if is_frozen() else Path(__file__).resolve().parents[3]


def default_stub() -> Path:
    """学生端模板（stub）的默认位置。

    冻结后**必须**看程序目录：发布包里 stub-template.exe 就放在
    ExamAdmin.exe 旁边。早期实现一律按源码目录结构去找
    （`__file__` 的 parents[3] + target/release），冻结后路径变成
    `.../ExamAdmin/target/release/exam-client.exe` —— 根本不存在，
    教师点「开始打包」只会看到"找不到 stub"。
    """
    name = "exam-client.exe" if os.name == "nt" else "exam-client"
    if is_frozen():
        d = program_dir()
        for cand in (
            d / f"stub-template{'.exe' if os.name == 'nt' else ''}",
            d / "_internal" / name,
        ):
            if cand.is_file():
                return cand
        # 都不在：返回"应该放的位置"，让报错信息有指导意义
        return d / f"stub-template{'.exe' if os.name == 'nt' else ''}"
    return builder_exe_root() / name


def _writable(d: Path) -> bool:
    """真正写一次来判断目录可写。

    不能用 `os.access`：它在 Windows 上对 ACL 的判断不可靠，
    而企业环境里桌面常被 OneDrive 重定向、权限也随之变化。
    """
    try:
        d.mkdir(parents=True, exist_ok=True)
        probe = d / ".exam_write_probe"
        probe.write_bytes(b"")
        probe.unlink()
        return True
    except OSError:
        return False


def default_out() -> Path:
    """打包输出位置的默认值：**第一个真正可写**的用户目录。

    硬编码桌面会踩坑：实测某些环境桌面被 OneDrive 接管，写入报
    「拒绝访问 (os error 5)」，教师点打包只看到一句错误。
    这里按"文档 → 桌面 → 用户目录 → 程序目录"顺序探测，
    都不可写才退回当前工作目录。
    """
    if not is_frozen():
        return Path(__file__).resolve().parents[3] / "dist" / "StudentClient.exe"

    home = Path.home()
    for d in (
        home / "Documents",
        home / "Desktop",
        home / "OneDrive" / "Desktop",
        home,
        program_dir(),
    ):
        if d.is_dir() and _writable(d):
            return d / "StudentClient.exe"
    return Path.cwd() / "StudentClient.exe"


def builder_exe_root() -> Path:
    """返回 client 产物所在目录（与 builder_exe 的搜索规则一致）。"""
    name = "exam-client.exe" if os.name == "nt" else "exam-client"
    return builder_exe().parent if builder_exe().name == name else _client_dir()


def _client_dir() -> Path:
    root = Path(__file__).resolve().parents[3]
    name = "exam-client.exe" if os.name == "nt" else "exam-client"
    for cand in (
        root / "target" / "release" / name,
        root / "target" / "x86_64-pc-windows-msvc" / "release" / name,
    ):
        if cand.is_file():
            return cand.parent
    for d in sorted((root / "target").glob("*/release")):
        if (d / name).is_file():
            return d
    return root / "target" / "release"


def builder_exe() -> Path:
    """定位 exam-builder 可执行文件。

    PyInstaller 打包后用 `sys._MEIPASS`（内嵌资源目录）；
    源码运行时回落到仓库的 `target/release`。
    """
    name = "exam-builder.exe" if os.name == "nt" else "exam-builder"
    base = getattr(sys, "_MEIPASS", None)
    if base:
        p = Path(base) / name
        if p.exists():
            return p
    root = Path(__file__).resolve().parents[3]  # admin/app/ui → 仓库根
    # cargo 产物可能在 target/release 或 target/<triple>/release，
    # 取决于构建时是否加了 --target。两种都要认。
    for cand in (
        root / "target" / "release" / name,
        root / "target" / "x86_64-pc-windows-msvc" / "release" / name,
    ):
        if cand.is_file():
            return cand
    for d in sorted((root / "target").glob("*/release")):
        if (d / name).is_file():
            return d / name
    return root / "target" / "release" / name


def parse_keygen_output(text: str) -> str:
    """校验 keygen 输出必须是 64 位十六进制。"""
    key = (text or "").strip()
    if len(key) != 64 or any(c not in "0123456789abcdefABCDEF" for c in key):
        raise ValueError(f"exam-builder 返回的内容不是 64 位十六进制：\n{key[:100]}")
    return key


class KeygenWorker(QThread):
    """后台生成主密钥：避免 subprocess 阻塞 GUI（客户反馈"像卡死"）。"""

    done = Signal(str)
    failed = Signal(str)

    def run(self) -> None:
        try:
            cp = subprocess.run(
                [str(builder_exe()), "keygen"],
                capture_output=True,
                text=True,
                encoding="utf-8",
                errors="replace",
                timeout=30,
                **no_window_kwargs(),
            )
        except Exception as e:  # noqa: BLE001 - 展示给用户
            self.failed.emit(str(e))
            return
        if cp.returncode != 0:
            detail = (cp.stderr or cp.stdout or f"退出码 {cp.returncode}").strip()
            self.failed.emit(detail[:300] or f"退出码 {cp.returncode}")
            return
        try:
            self.done.emit(parse_keygen_output(cp.stdout))
        except ValueError as e:
            self.failed.emit(str(e))


class BuildWorker(QThread):
    log = Signal(str)
    done = Signal(bool, str)  # (成功, 结果说明)

    def __init__(
        self,
        manifest_path: Path,
        banks_dir: Path,
        stub: Path,
        out: Path,
        master_hex: str,
        keyfile: Path | None,
        pin_login: bool = False,
        emit_pins: Path | None = None,
    ) -> None:
        super().__init__()
        self.manifest_path = manifest_path
        self.banks_dir = banks_dir
        self.stub = stub
        self.out = out
        self.master_hex = master_hex
        self.keyfile = keyfile
        self.pin_login = pin_login
        self.emit_pins = emit_pins
        self._proc: subprocess.Popen[str] | None = None

    def run(self) -> None:  # noqa: D401 - QThread 约定
        # 命令拼装与密钥传递在 app/core/buildcmd.py 里（Qt-free、有单测）：
        # 主密钥**只走环境变量**，绝不进命令行。
        cmd = build_command(
            builder_exe=builder_exe(),
            manifest=self.manifest_path,
            banks_dir=self.banks_dir,
            stub=self.stub,
            out=self.out,
            keyfile=self.keyfile,
            pin_login=self.pin_login,
            emit_pins=self.emit_pins,
        )
        env = build_env(self.master_hex)

        try:
            self._proc = subprocess.Popen(
                cmd,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                encoding="utf-8",
                errors="replace",
                env=env,
                cwd=str(self.manifest_path.parent),
                **no_window_kwargs(),
            )
        except OSError as e:
            self.master_hex = ""
            self.done.emit(False, f"无法启动打包程序：{e}")
            return

        assert self._proc.stdout is not None
        try:
            for line in self._proc.stdout:
                # 安全兜底：万一某行带了 master（不应发生），打码后再显示
                self.log.emit(_redact(line.rstrip(), self.master_hex))
            code = self._proc.wait()
        finally:
            # master 用完立即丢弃：worker 对象会被 self.worker 一直持有，
            # 不清空就等于把主密钥留在内存里直到下次打包。
            self.master_hex = ""
        if code == 0:
            self.done.emit(True, str(self.out))
        else:
            self.done.emit(False, f"打包程序退出码 {code}")

    def cancel(self) -> None:
        if self._proc and self._proc.poll() is None:
            self._proc.terminate()


def _redact(text: str, secret: str) -> str:
    if secret and secret in text:
        return text.replace(secret, "<已隐藏>")
    return text


class BuildTab(QWidget):
    #: 回看策略/配置变化，通知主窗口标记工程未保存
    changed = Signal()
    #: 打包成功（参数为输出文件路径）——主窗口据此自动保存工程
    built = Signal(str)

    def __init__(self, state: ProjectState) -> None:
        super().__init__()
        self.state = state
        self.worker: BuildWorker | None = None
        #: 本次打包用的临时目录，结束后必须删除（里面有题目原文）
        self._tmpdir: Path | None = None
        #: 用户是否点了「取消」。用来把"主动取消"和"真的失败"分开——
        #: 取消时进程退出码同样是 1，只看退出码会弹「打包失败」，
        #: 教师会以为程序坏了。
        self._cancelled = False
        self._build_ui()
        self.refresh_preview()

    def _build_ui(self) -> None:
        lay = QVBoxLayout(self)

        # 配置区
        box = QGroupBox("打包参数")
        form = QFormLayout(box)

        self.edit_master = QLineEdit()
        self.edit_master.setEchoMode(QLineEdit.EchoMode.Password)
        self.edit_master.setPlaceholderText("64 位十六进制主密钥")
        row = QHBoxLayout()
        row.addWidget(self.edit_master, 1)
        self.btn_gen = QPushButton("生成")
        self.btn_gen.setToolTip("调用 exam-builder keygen 生成一个随机主密钥")
        self.btn_gen.clicked.connect(self.gen_master)
        row.addWidget(self.btn_gen)
        self.gen_progress = QProgressBar()
        self.gen_progress.setRange(0, 0)  # 不确定进度：显示"正在跑"
        self.gen_progress.setFixedWidth(96)
        self.gen_progress.setVisible(False)
        self.gen_progress.setTextVisible(False)
        row.addWidget(self.gen_progress)
        # 复制按钮：主密钥的**唯一**导出方式。
        # 不写进日志——日志会被截图、会留在共享电脑上，而剪贴板由教师自己控制。
        btn_copy = QPushButton("复制主密钥")
        btn_copy.setToolTip("把主密钥复制到剪贴板，粘贴到密码管理器里保存")
        btn_copy.clicked.connect(self.copy_master)
        row.addWidget(btn_copy)
        btn_backup = QPushButton("备份")
        btn_backup.setToolTip("把主密钥加密备份成 .eekey 文件（建议存到密码管理器/U 盘）")
        btn_backup.clicked.connect(self.backup_master)
        row.addWidget(btn_backup)
        btn_restore = QPushButton("恢复")
        btn_restore.setToolTip("从 .eekey 备份恢复主密钥")
        btn_restore.clicked.connect(self.restore_master)
        row.addWidget(btn_restore)
        # 显式的"清空"：教师自己决定何时抹掉，比定时器更可控。
        btn_clear = QPushButton("清空")
        btn_clear.setToolTip("立即清空输入框里的主密钥（请确认已保存到密码管理器）")
        btn_clear.clicked.connect(self.clear_master)
        row.addWidget(btn_clear)
        form.addRow("主密钥", row)
        self.lbl_key_fp = QLabel("指纹：—")
        self.lbl_key_fp.setStyleSheet("color: #888; font-size: 11px;")
        form.addRow("主密钥指纹", self.lbl_key_fp)
        self.edit_master.textChanged.connect(self._update_key_fp)

        self.copy_hint = QLabel("")
        self.copy_hint.setWordWrap(True)
        form.addRow("", self.copy_hint)

        self.edit_stub = QLineEdit(self._default_stub())
        row2 = QHBoxLayout()
        row2.addWidget(self.edit_stub, 1)
        btn_stub = QPushButton("选择")
        btn_stub.clicked.connect(lambda: self._pick_file(self.edit_stub, "选择学生端 stub"))
        row2.addWidget(btn_stub)
        form.addRow("学生端 stub", row2)

        self.edit_out = QLineEdit(str(self._default_out()))
        row3 = QHBoxLayout()
        row3.addWidget(self.edit_out, 1)
        btn_out = QPushButton("另存为")
        btn_out.clicked.connect(self._pick_save)
        row3.addWidget(btn_out)
        form.addRow("输出包", row3)

        from app.ui.app_settings import project_dir_display

        self.edit_projdir = QLineEdit()
        self.edit_projdir.setText(str(project_dir_display()))
        self.edit_projdir.setToolTip(
            "当前工程会保存到这个目录；新建工程时也用它。含标准答案，勿分发。"
        )
        self.edit_projdir.editingFinished.connect(self._apply_project_dir)
        row_pd = QHBoxLayout()
        row_pd.addWidget(self.edit_projdir, 1)
        btn_pd = QPushButton("浏览")
        btn_pd.clicked.connect(self._browse_project_dir)
        row_pd.addWidget(btn_pd)
        btn_pd_reset = QPushButton("默认")
        btn_pd_reset.setToolTip("恢复为程序目录下的 projects\\")
        btn_pd_reset.clicked.connect(self._reset_project_dir)
        row_pd.addWidget(btn_pd_reset)
        form.addRow("工程保存位置", row_pd)

        self.lbl_current = QLabel("未打开（打包时新建工程）")
        self.lbl_current.setStyleSheet("color: #888;")
        self.lbl_current.setWordWrap(True)
        form.addRow("当前工程", self.lbl_current)

        self.chk_pin_login = QCheckBox("每生 PIN 登录（生成 6 位 PIN，导出 CSV 发给学生）")
        self.chk_pin_login.setToolTip(
            "包内不存 PIN 表；打包成功后选择 CSV 保存文件夹，学生凭学号+PIN 解锁"
        )
        form.addRow("登录方式", self.chk_pin_login)

        self.chk_student_review = QCheckBox("仅教师可查看原卷（学生端不提供免授权回看）")
        self.chk_student_review.setToolTip(
            "勾选（默认）：学生端无回看入口，考后由教师用 master 查看原卷；\n"
            "不勾选：学生已交卷且未到结束时间可免授权只读回看。"
        )
        self.chk_student_review.setChecked(self.state.student_review != "free")
        self.chk_student_review.toggled.connect(self._on_student_review_toggled)
        form.addRow("学生回看", self.chk_student_review)

        from app.ui.app_settings import PROJECT_ENCRYPT, settings

        self.chk_encrypt = QCheckBox("加密保存工程（含标准答案，建议开启）")
        self.chk_encrypt.setChecked(settings().value(PROJECT_ENCRYPT, True, type=bool))
        self.chk_encrypt.toggled.connect(lambda on: settings().setValue(PROJECT_ENCRYPT, bool(on)))
        form.addRow("工程加密", self.chk_encrypt)

        self.cb_time_mode = QComboBox()
        self.cb_time_mode.addItem("关闭（仍用 HTTPS Date）", "off")
        self.cb_time_mode.addItem("优先：有令牌就用，失败回退", "prefer")
        self.cb_time_mode.addItem("必须：没有有效令牌就不开考", "require")
        idx = self.cb_time_mode.findData(self.state.time_mode or "off")
        self.cb_time_mode.setCurrentIndex(max(0, idx))
        form.addRow("时间令牌", self.cb_time_mode)
        self.edit_time_url = QLineEdit(self.state.time_url)
        self.edit_time_url.setPlaceholderText(
            "https://time.example.com/time（require/prefer 时必填）"
        )
        form.addRow("令牌 URL", self.edit_time_url)
        self.edit_time_pubkey = QLineEdit(self.state.time_pubkey)
        self.edit_time_pubkey.setPlaceholderText("base64 Ed25519 公钥（32 字节）")
        form.addRow("令牌公钥", self.edit_time_pubkey)
        self.spin_time_skew = QSpinBox()
        self.spin_time_skew.setRange(30, 3600)
        self.spin_time_skew.setValue(int(self.state.time_max_skew or 300))
        self.spin_time_skew.setSuffix(" 秒")
        self.spin_time_skew.setToolTip(
            "仅用于日志警告：本机时钟可被学生修改，不能作为放行/拒绝依据"
        )
        form.addRow("时钟偏差警告阈值", self.spin_time_skew)
        # 改完就同步到 state，保证"保存工程"也会带上时间令牌配置
        self.cb_time_mode.currentIndexChanged.connect(self._sync_time_config)
        self.edit_time_url.textChanged.connect(self._sync_time_config)
        self.edit_time_pubkey.textChanged.connect(self._sync_time_config)
        self.spin_time_skew.valueChanged.connect(self._sync_time_config)

        self.edit_keyfile = QLineEdit()
        self.edit_keyfile.setPlaceholderText("留空则不导出 K_manifest")
        row4 = QHBoxLayout()
        row4.addWidget(self.edit_keyfile, 1)
        btn_kf = QPushButton("选择")
        btn_kf.clicked.connect(
            lambda: self._pick_file(self.edit_keyfile, "K_manifest 保存位置", save=True)
        )
        row4.addWidget(btn_kf)
        form.addRow("导出 K_manifest", row4)
        for btn in (
            btn_copy,
            btn_backup,
            btn_restore,
            btn_stub,
            btn_out,
            btn_pd,
            btn_kf,
        ):
            fit_button_text(btn)

        lay.addWidget(box)

        # 启动自愈：字段里的路径若不存在、而默认位置有货，自动换过去。
        # 这样即使界面上的默认值因为环境变化（如换了机器/目录）失效，
        # 教师也不必自己去翻 stub-template.exe。
        self._autofix_paths()

        self.preview = QLabel()
        self.preview.setWordWrap(True)
        lay.addWidget(self.preview)

        # 操作区
        ops = QHBoxLayout()
        self.btn_build = QPushButton("开始打包")
        self.btn_build.clicked.connect(self.start_build)
        self.btn_cancel = QPushButton("取消")
        self.btn_cancel.setEnabled(False)
        self.btn_cancel.clicked.connect(self.cancel_build)
        ops.addWidget(self.btn_build)
        ops.addWidget(self.btn_cancel)
        ops.addStretch(1)
        lay.addLayout(ops)

        self.progress = QProgressBar()
        self.progress.setRange(0, 0)  # 不确定态
        self.progress.setVisible(False)
        lay.addWidget(self.progress)

        self.log_view = QPlainTextEdit()
        self.log_view.setReadOnly(True)
        self.log_view.setPlaceholderText("打包日志会显示在这里")
        lay.addWidget(self.log_view, 1)

    def _autofix_paths(self) -> None:
        """把不存在的默认路径纠正到实际可用的位置。"""
        stub_field = self.edit_stub.text().strip()
        if stub_field and not Path(stub_field).is_file():
            cand = default_stub()
            if cand.is_file():
                self.edit_stub.setText(str(cand))
        out_field = self.edit_out.text().strip()
        if not out_field:
            self.edit_out.setText(str(default_out()))

    # ---------------------------------------------------------------- 默认值

    def _default_stub(self) -> str:
        return str(default_stub())

    def _default_out(self) -> Path:
        return default_out()

    # ---------------------------------------------------------------- 交互

    def _pick_file(self, target: QLineEdit, title: str, save: bool = False) -> None:
        if save:
            p, _ = QFileDialog.getSaveFileName(self, title, target.text())
        else:
            p, _ = QFileDialog.getOpenFileName(self, title, target.text())
        if p:
            target.setText(p)

    def _pick_save(self) -> None:
        p, _ = QFileDialog.getSaveFileName(
            self, "保存学生端包", self.edit_out.text(), "可执行文件 (*.exe);;所有文件 (*)"
        )
        if p:
            self.edit_out.setText(p)

    #: 复制成功后多久自动清空输入框（毫秒）。
    #:
    #: 留着密钥的好处是"打包完还能再复制一次"；坏处是窗口留在共享电脑上时
    #: 任何人点一下按钮就能拿走。5 分钟够教师打开密码管理器粘贴，之后自动抹掉。
    MASTER_AUTO_CLEAR_MS = 5 * 60 * 1000

    def copy_master(self) -> None:
        """把主密钥复制到剪贴板并给出确认提示。

        这是 master 的**唯一**导出途径（日志里绝不显示它）：
        剪贴板的内容由教师自己决定粘贴到哪，不会像日志那样被动留存。
        """
        key = self.edit_master.text().strip()
        if not key:
            self.copy_hint.setStyleSheet("color:#c04040")
            self.copy_hint.setText("⚠ 还没有主密钥：请先点【生成】，或用已有的密钥填入。")
            return

        clip = QGuiApplication.clipboard()
        clip.setText(key)
        # 回读校验：远程桌面、剪贴板被其他程序独占时 setText 会静默失败。
        # 不校验就会骗教师"已复制"，等人走了才发现剪贴板是空的。
        if clip.text() != key:
            self.copy_hint.setStyleSheet("color:#c04040")
            self.copy_hint.setText("✗ 复制失败：剪贴板被其他程序占用。请手动选中密钥后按 Ctrl+C。")
            return

        self.copy_hint.setStyleSheet("color:#2e7d32")
        self.copy_hint.setText(
            "✔ 主密钥已复制到剪贴板，请妥善保管（不要发给学生、不要留在共享电脑上）。"
        )
        QTimer.singleShot(self.MASTER_AUTO_CLEAR_MS, lambda: self._clear_master_if_unchanged(key))

    def _clear_master_if_unchanged(self, key: str) -> None:
        """到点自动清空主密钥输入框。

        两道保险：
        1. 内容被教师改过 → 不动；
        2. **剪贴板里已经不是这把密钥了** → 也不动。

        第 2 条是关键：主密钥通常只有"输入框 + 剪贴板"两份。若剪贴板已被
        别的东西覆盖，输入框就是唯一副本——清掉等于这个场次再也打不出包。
        """
        if self.edit_master.text().strip() != key:
            return
        if QGuiApplication.clipboard().text() != key:
            self.copy_hint.setStyleSheet("color:#9a6a00")
            self.copy_hint.setText(
                "ℹ 剪贴板内容已变化，主密钥保留在输入框中（未自动清空）。请尽快保存。"
            )
            return
        self.edit_master.clear()
        self.copy_hint.setStyleSheet("color:#2e7d32")
        self.copy_hint.setText("✔ 主密钥已从输入框自动清空（复制早已完成，请从剪贴板粘贴保存）。")

    def clear_master(self) -> None:
        """教师主动清空输入框（不碰剪贴板）。"""
        if not self.edit_master.text().strip():
            self.copy_hint.setStyleSheet("color:#c04040")
            self.copy_hint.setText("输入框里没有主密钥。")
            return
        self.edit_master.clear()
        self.copy_hint.setStyleSheet("color:#2e7d32")
        self.copy_hint.setText("✔ 已清空输入框中的主密钥。若剪贴板里还有，请尽快保存到密码管理器。")

    def gen_master(self) -> None:
        exe = builder_exe()
        if not exe.exists():
            QMessageBox.critical(
                self,
                "找不到 exam-builder",
                f"{exe}\n\n请先 cargo build --release -p builder",
            )
            return
        keygen = getattr(self, "_keygen", None)
        try:
            if keygen is not None and keygen.isRunning():
                return
        except RuntimeError:
            # 上一轮 finished→deleteLater 后 C++ 对象已删，别在它上面调 isRunning
            self._keygen = None
        self.btn_gen.setEnabled(False)
        self.btn_gen.setText("生成中…")
        self.gen_progress.setVisible(True)
        self.copy_hint.setStyleSheet("color:#9a6a00")
        self.copy_hint.setText("正在生成主密钥…（通常 1–2 秒）")
        self._keygen = KeygenWorker(self)
        self._keygen.done.connect(self._on_keygen_done)
        self._keygen.failed.connect(self._on_keygen_failed)
        self._keygen.finished.connect(self._keygen.deleteLater)
        self._keygen.start()

    def _on_keygen_done(self, key: str) -> None:
        self._reset_keygen_ui()
        self.edit_master.setText(key)
        self.copy_hint.setStyleSheet("color:#2e7d32")
        self.copy_hint.setText("✔ 已生成主密钥。请立刻点【复制主密钥】保存到密码管理器。")

    def _on_keygen_failed(self, message: str) -> None:
        self._reset_keygen_ui()
        QMessageBox.critical(self, "生成失败", message)

    def _reset_keygen_ui(self) -> None:
        self._keygen = None
        self.btn_gen.setEnabled(True)
        self.btn_gen.setText("生成")
        self.gen_progress.setVisible(False)

    def _update_key_fp(self) -> None:
        key = self.edit_master.text().strip()
        if len(key) == 64 and all(c in "0123456789abcdefABCDEF" for c in key):
            fp = hashlib.blake2s(key.lower().encode(), digest_size=4).hexdigest()
            self.lbl_key_fp.setText(f"指纹：{fp}")
        else:
            self.lbl_key_fp.setText("指纹：—")

    def _ask_passphrase(self, title: str, confirm: bool = False) -> str | None:
        while True:
            pw, ok = QInputDialog.getText(self, title, "口令：", QLineEdit.EchoMode.Password)
            if not ok:
                return None
            if not pw:
                QMessageBox.warning(self, title, "口令不能为空。")
                continue
            if confirm:
                pw2, ok2 = QInputDialog.getText(
                    self, title, "再次输入口令：", QLineEdit.EchoMode.Password
                )
                if not ok2:
                    return None
                if pw != pw2:
                    QMessageBox.warning(self, title, "两次输入的口令不一致。")
                    continue
            return pw

    def backup_master(self) -> None:
        key = self.edit_master.text().strip()
        if not key:
            QMessageBox.information(self, "没有主密钥", "请先生成或填入主密钥。")
            return
        pw = self._ask_passphrase("设置备份口令", confirm=True)
        if pw is None:
            return
        path, _ = QFileDialog.getSaveFileName(
            self, "备份主密钥", "主密钥备份.eekey", "密钥备份 (*.eekey)"
        )
        if not path:
            return
        try:
            from app.core import secure

            secure.backup_master_key(key, Path(path), pw)
        except Exception as e:  # noqa: BLE001
            QMessageBox.critical(self, "备份失败", str(e))
            return
        QMessageBox.information(
            self, "已备份", f"主密钥已加密备份到：\n{path}\n\n请把口令另行保存。"
        )

    def restore_master(self) -> None:
        path, _ = QFileDialog.getOpenFileName(
            self, "恢复主密钥", "", "密钥备份 (*.eekey);;全部文件 (*)"
        )
        if not path:
            return
        pw = self._ask_passphrase("输入备份口令")
        if pw is None:
            return
        try:
            from app.core import secure

            key = secure.restore_master_key(Path(path), pw)
        except Exception as e:  # noqa: BLE001
            QMessageBox.critical(self, "恢复失败", str(e))
            return
        self.edit_master.setText(key)
        QMessageBox.information(self, "已恢复", "主密钥已填入输入框，请核对指纹。")

    def refresh_preview(self) -> None:
        """节流：validate() 含精确难度 DP，大题库时别在每个事件里跑。"""
        if not hasattr(self, "_preview_timer"):
            self._preview_timer = QTimer(self)
            self._preview_timer.setSingleShot(True)
            self._preview_timer.timeout.connect(self._refresh_preview_now)
        self._preview_timer.start(250)

    def _refresh_preview_now(self) -> None:
        total = sum(len(s.students) for s in self.state.sessions)
        self._sync_time_config()
        problems = self.state.validate()
        if problems:
            self.preview.setText(
                f"<b>配置有 {len(problems)} 个问题，打包会失败：</b><br>"
                + "<br>".join(f"· {p}" for p in problems[:6])
            )
            self.preview.setStyleSheet("color:#b03030;")
        else:
            self.preview.setText(
                f"共 {len(self.state.sessions)} 个场次、{total} 个学生信封，可以打包。\n"
                "密钥会自动写进包内 —— 学生双击 exe、输入学号即可，"
                "不需要任何密钥文件，也不需要你重新编译程序。"
            )
            self.preview.setStyleSheet("color:#2a7a2a;")

    # ---------------------------------------------------------------- 打包

    def _on_student_review_toggled(self, on: bool) -> None:
        self.state.student_review = "none" if on else "free"
        self.changed.emit()

    def _sync_time_config(self) -> None:
        self.state.student_review = "none" if self.chk_student_review.isChecked() else "free"
        self.state.time_mode = str(self.cb_time_mode.currentData() or "off")
        self.state.time_url = self.edit_time_url.text().strip()
        self.state.time_pubkey = self.edit_time_pubkey.text().strip()
        self.state.time_max_skew = int(self.spin_time_skew.value())

    def start_build(self) -> None:
        if self.worker is not None and self.worker.isRunning():
            return

        self._sync_time_config()
        problems = self.state.validate()
        if problems:
            QMessageBox.critical(self, "配置有问题", "\n".join(f"· {p}" for p in problems))
            return

        master = self.edit_master.text().strip()
        if len(master) != 64 or any(c not in "0123456789abcdefABCDEF" for c in master):
            QMessageBox.critical(self, "主密钥非法", "主密钥必须是 64 位十六进制字符串。")
            return

        stub = Path(self.edit_stub.text())
        if not stub.exists():
            near = default_stub()
            hint = (
                f"\n\n发布包里的模板应该在：\n{near}"
                if is_frozen()
                else "\n\n源码运行时需要先 cargo build --release -p client"
            )
            QMessageBox.critical(
                self,
                "找不到学生端模板",
                f"{stub}\n\n这个文件不存在。{hint}\n\n可以点右侧「选择…」指定 stub-template.exe。",
            )
            return

        out = Path(self.edit_out.text())
        # 输出文件预检：Windows 上正在运行/被杀毒软件/OneDrive 占用的 exe
        # 无法删除，Builder 会在最后一步报 os error 5。提前发现并让老师另存。
        if out.exists() and not self._can_write_existing(out):
            suggested = out.with_name(f"{out.stem}-{datetime.now():%Y%m%d-%H%M%S}{out.suffix}")
            QMessageBox.warning(
                self,
                "输出文件被占用",
                f"{out}\n\n"
                "这个文件当前无法覆盖（可能正在运行、被占用或只读）。\n"
                "请先关闭正在运行的学生端；也可以直接另存为新文件。",
            )
            new_path, _ = QFileDialog.getSaveFileName(
                self, "另存学生端", str(suggested), "EXE (*.exe)"
            )
            if not new_path:
                return
            out = Path(new_path)
            self.edit_out.setText(str(out))
        # 覆盖确认：builder 默认拒绝覆盖，这里先问清楚再放行。
        if out.exists():
            ret = QMessageBox.question(
                self,
                "覆盖已有文件",
                f"{out}\n\n这个文件已存在，继续会覆盖它。要继续吗？",
                QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
            )
            if ret != QMessageBox.StandardButton.Yes:
                return

        try:
            out.parent.mkdir(parents=True, exist_ok=True)
            probe = out.parent / ".exam_write_probe"
            probe.write_bytes(b"")
            probe.unlink()
        except OSError as e:
            QMessageBox.critical(
                self,
                "输出目录不可写",
                f"{out.parent}\n\n{e}\n\n请换一个保存位置（例如桌面或文档目录）。",
            )
            return

        # 题库目录：各题库路径的公共父目录太多变，改用临时目录扁平复制。
        #
        # ⚠ 安全：这些临时文件里是**题目原文**。旧实现直接
        # `bank.model_dump_json()` 落盘，会把 `answer` 字段一起写出去，
        # 而且打包结束后从不删除——等于把标准答案永久留在 %TEMP% 里。
        # 现在做两件事：
        #   1. 写盘前剥掉 answer（builder 本来也会剥，这里提前剥是纵深防御）；
        #   2. 打包结束（成功/失败/取消）一律删除临时目录。
        tmpdir = Path(tempfile.mkdtemp(prefix="examadm-"))
        self._tmpdir = tmpdir
        try:
            write_temp_banks(self.state.banks, tmpdir)
            manifest_path = tmpdir / "manifest.json"
            manifest_path.write_text(
                json.dumps(
                    json.loads(self.state.build_manifest().model_dump_json()),
                    ensure_ascii=False,
                    indent=2,
                ),
                encoding="utf-8",
            )
        except OSError as e:
            self._cleanup_tmp()
            QMessageBox.critical(self, "准备打包数据失败", str(e))
            return

        keyfile = Path(self.edit_keyfile.text()) if self.edit_keyfile.text().strip() else None

        self.log_view.clear()
        self.copy_hint.setText("")
        self._cancelled = False
        self._append(f"题库目录 {tmpdir}")
        self._append(f"清单 {manifest_path}")
        self.btn_build.setEnabled(False)
        self.btn_cancel.setEnabled(True)
        self.progress.setVisible(True)

        pin_login = self.chk_pin_login.isChecked()
        self._pin_path: Path | None = None
        self._pin_temp: Path | None = None
        self._pin_stamp = datetime.now().strftime("%Y%m%d-%H%M%S")
        emit_pins = None
        if pin_login:
            # 先写到临时目录；打包成功后由老师选择保存文件夹，再复制出去。
            emit_pins = tmpdir / "pins.csv"
            self._pin_temp = emit_pins
            self._append("PIN 表将在打包成功后询问保存文件夹")
        self.worker = BuildWorker(
            manifest_path, tmpdir, stub, out, master, keyfile, pin_login, emit_pins
        )
        self.worker.log.connect(self._append)
        self.worker.done.connect(self._on_done)
        # 故意**不清空**主密钥输入框：教师打包完还要用它（复制保存）。
        # 输入框是密码回显，且这是教师自己的电脑。
        self.worker.start()

    def _remove_partial_output(self) -> None:
        """删掉被打断时留下的 `<输出名>.tmp`。

        builder 是"先写 .tmp 再 rename"，进程被 terminate 时 .tmp 会留下来。
        它不会影响下次打包（rename 目标是最终文件），但对教师是个莫名其妙的
        陌生文件，顺手清掉。
        """
        out = self.edit_out.text().strip()
        if not out:
            return
        partial = Path(f"{out}.tmp")
        try:
            if partial.is_file():
                partial.unlink()
                self._append(f"  已清理半成品 {partial.name}")
        except OSError:
            pass

    def _cleanup_tmp(self) -> None:
        """删除打包用的临时目录。

        里面有题目原文（虽已剥掉 answer），不该留在磁盘上。
        `ignore_errors=True`：删除失败也不该让打包流程报错。
        """
        if self._tmpdir is not None:
            shutil.rmtree(self._tmpdir, ignore_errors=True)
            self._tmpdir = None

    def cancel_build(self) -> None:
        if self.worker:
            self._cancelled = True
            self.worker.cancel()
            self._append("… 已请求取消")

    def log_line(self, line: str) -> None:
        """供主窗口写入打包日志（如"工程已保存"）。"""
        self._append(line)

    def set_project_dir(self, path) -> None:
        self.edit_projdir.setText(str(path))

    def set_current_project(self, path) -> None:
        if path is None:
            self.lbl_current.setText("未打开（打包时新建工程）")
            self.lbl_current.setToolTip("")
        else:
            self.lbl_current.setText(f"{path.name}（{path.parent}）")
            self.lbl_current.setToolTip(str(path))

    def commit_project_dir(self) -> None:
        """打包前把输入框里的目录落盘（防止没触发 editingFinished）。"""
        self._apply_project_dir()

    def _browse_project_dir(self) -> None:
        from PySide6.QtWidgets import QFileDialog

        d = QFileDialog.getExistingDirectory(self, "选择工程保存位置", self.edit_projdir.text())
        if d:
            self.edit_projdir.setText(d)
            self._apply_project_dir()

    def _apply_project_dir(self) -> None:
        from PySide6.QtWidgets import QMessageBox

        from app.ui.app_settings import PROJECT_DIR, project_dir_display, settings

        raw = self.edit_projdir.text().strip()
        if not raw:
            self._reset_project_dir()
            return
        p = Path(raw).expanduser().resolve()
        try:
            p.mkdir(parents=True, exist_ok=True)
            probe = p / ".ees-write-test"
            probe.write_text("", encoding="utf-8")
            probe.unlink()
        except OSError as e:
            QMessageBox.warning(self, "目录不可用", f"{p}\n\n{e}")
            self.edit_projdir.setText(str(project_dir_display()))
            return
        self.edit_projdir.setText(str(p))
        settings().setValue(PROJECT_DIR, str(p))

    def _reset_project_dir(self) -> None:
        from app.ui.app_settings import PROJECT_DIR, project_dir_display, settings

        settings().remove(PROJECT_DIR)
        self.edit_projdir.setText(str(project_dir_display()))

    def _append(self, line: str) -> None:
        self.log_view.appendPlainText(line)

    @staticmethod
    def _can_write_existing(path: Path) -> bool:
        try:
            with path.open("r+b"):
                return True
        except OSError:
            return False

    @staticmethod
    def _pin_target_error(out: Path, target: Path, tmpdir: Path | None = None) -> str | None:
        """校验 PIN 表保存路径；返回 None 表示可用，否则返回给老师看的原因。"""
        try:
            out_dir = out.resolve().parent
            tgt = target.resolve()
        except OSError:
            return "路径无法解析，请换一个位置。"
        if target.exists() and target.is_dir():
            return "这是目录，不是 CSV 文件。"
        if tmpdir is not None:
            try:
                if tgt.is_relative_to(tmpdir.resolve()):
                    return "PIN 表不能放在打包临时目录，打包结束会被清理。"
            except OSError:
                pass
        if tgt == out_dir or out_dir in tgt.parents:
            return "PIN 表不能放在学生端 exe 所在目录或其子目录，学生能看到。"
        if os.path.normcase(str(tgt)) == os.path.normcase(str(out_dir)):
            return "PIN 表不能放在学生端 exe 所在目录。"
        if target.exists() and not BuildTab._can_write_existing(target):
            return "目标文件正在使用或只读，请关闭占用它的程序或换一个文件名。"
        try:
            target.parent.mkdir(parents=True, exist_ok=True)
            probe = target.parent / f".ees-pin-write-test-{os.getpid()}"
            probe.write_text("", encoding="utf-8")
            probe.unlink()
        except OSError as e:
            return f"该目录不可写：{e}"
        return None

    def _save_pin_csv(self, out: Path) -> Path | None:
        """把临时 PIN 表保存到老师选择的位置。

        不再静默回退到默认目录：选错/不可写就提示并重新选择；
        老师取消时暂存到系统临时目录，并明确显示路径。
        """
        src = getattr(self, "_pin_temp", None)
        if src is None or not Path(src).is_file():
            return None
        from app.ui.app_settings import project_dir_display, settings_path

        default_dir, _ = safe_pin_dir(
            project_dir_display(), out, settings_path().parent / "projects"
        )
        stamp = getattr(self, "_pin_stamp", "")
        default_file = default_dir / f"登录PIN-{out.stem}-{stamp}.csv"

        while True:
            chosen, _ = QFileDialog.getSaveFileName(
                self,
                "保存 PIN 表（不要放在考试包目录）",
                str(default_file),
                "CSV (*.csv)",
            )
            if not chosen:
                stashed = self._stash_pin_temp(Path(src))
                QMessageBox.warning(
                    self,
                    "PIN 表未保存",
                    f"已暂存到：\n{stashed}\n\n请手动复制到安全位置；临时文件不会自动删除。",
                )
                return stashed

            target = Path(chosen)
            if target.suffix.lower() != ".csv":
                target = target.with_suffix(".csv")
            reason = self._pin_target_error(out, target, self._tmpdir)
            if reason:
                QMessageBox.warning(
                    self,
                    "不能保存到这个位置",
                    f"{reason}\n\n请重新选择保存位置。\n建议保存到：\n{default_dir}",
                )
                continue

            try:
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(src, target)
                return target
            except OSError as e:
                QMessageBox.critical(
                    self,
                    "PIN 表保存失败",
                    f"{target}\n\n{e}\n\n请重新选择可写目录。",
                )
                # 继续循环，让老师重新选；不静默改存默认目录
                continue

    def _stash_pin_temp(self, src: Path) -> Path:
        """复制失败时只保留 PIN 文件，避免把整个题库临时目录留在磁盘。"""
        import tempfile

        dest = Path(tempfile.gettempdir()) / f"ees-pin-{getattr(self, '_pin_stamp', 'unknown')}.csv"
        try:
            shutil.copy2(src, dest)
            return dest
        except OSError:
            return src

    def _on_done(self, ok: bool, info: str) -> None:
        self.btn_build.setEnabled(True)
        self.btn_cancel.setEnabled(False)
        self.progress.setVisible(False)

        if not ok:
            if self._cancelled:
                # 主动取消不是故障，别用红色错误框吓人
                self._remove_partial_output()
                self._append("… 已取消打包（未生成文件）")
                QMessageBox.information(
                    self,
                    "已取消",
                    "已取消打包，没有生成文件。\n如需重试，改好参数后再点【开始打包】。",
                )
                self._cancelled = False
                self._cleanup_tmp()
                return
            self._append(f"✗ 失败：{info}")
            QMessageBox.critical(self, "打包失败", info)
            self._cleanup_tmp()
            return

        out = Path(info)
        self._append(f"✔ 成功：{out}")
        self.built.emit(str(out))
        try:
            size = out.stat().st_size
            digest = hashlib.sha256(out.read_bytes()).hexdigest()
            self._append(f"  大小 {size:,} 字节")
            self._append(f"  SHA-256 {digest}")
        except OSError:
            pass
        self._pin_path = self._save_pin_csv(out)
        if self._pin_path is None and getattr(self, "_pin_temp", None) is not None:
            QMessageBox.warning(
                self,
                "PIN 表未生成",
                "打包成功，但没有找到 PIN 表文件。\n"
                "请检查打包日志并重试；学生暂时无法用 PIN 登录。",
            )
        if self._pin_path is None or self._pin_path != self._pin_temp:
            self._cleanup_tmp()
        pin_note = ""
        if getattr(self, "_pin_path", None) is not None and self._pin_path.exists():
            pin_note = f"\n\nPIN 表：\n{self._pin_path}"
        box = QMessageBox(self)
        box.setWindowTitle("打包成功")
        box.setIcon(QMessageBox.Icon.Information)
        box.setText(
            f"已生成：\n{out}\n\n"
            "✔ 密钥已内嵌在包内，学生双击即可使用（无需任何参数）。\n\n"
            "⚠ 请点上方【复制主密钥】保存到密码管理器——\n"
            "  它是重新打包本场考试的唯一凭据。\n\n"
            "请核对日志里的 SHA-256。\n\n"
            "注意：重新生成 exe 不会重置学生本机已交卷答题卡；\n"
            "补考请新建场次/更换 salt。" + pin_note
        )
        if pin_note:
            box.setInformativeText("PIN 表只发给教师，未放进考试包目录。")
        open_out = box.addButton("打开输出目录", QMessageBox.ButtonRole.ActionRole)
        open_pin = None
        if pin_note:
            open_pin = box.addButton("打开 PIN 目录", QMessageBox.ButtonRole.ActionRole)
        box.addButton("确定", QMessageBox.ButtonRole.AcceptRole)
        box.exec()
        from PySide6.QtGui import QDesktopServices

        if box.clickedButton() is open_out:
            QDesktopServices.openUrl(QUrl.fromLocalFile(str(out.parent)))
        elif open_pin is not None and box.clickedButton() is open_pin:
            QDesktopServices.openUrl(QUrl.fromLocalFile(str(self._pin_path.parent)))
