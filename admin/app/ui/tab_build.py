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
from pathlib import Path

from PySide6.QtCore import QThread, Signal
from PySide6.QtWidgets import (
    QFileDialog,
    QFormLayout,
    QGroupBox,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QMessageBox,
    QPlainTextEdit,
    QProgressBar,
    QPushButton,
    QVBoxLayout,
    QWidget,
)

from app.core.banks_io import write_temp_banks
from app.core.state import ProjectState


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
    ) -> None:
        super().__init__()
        self.manifest_path = manifest_path
        self.banks_dir = banks_dir
        self.stub = stub
        self.out = out
        self.master_hex = master_hex
        self.keyfile = keyfile
        self._proc: subprocess.Popen[str] | None = None

    def run(self) -> None:  # noqa: D401 - QThread 约定
        # 主密钥**只走环境变量** EXAM_MASTER，绝不进命令行——
        # 命令行参数会被同机其他用户从进程列表里看到。
        cmd = [
            str(builder_exe()),
            "build",
            "--manifest",
            str(self.manifest_path),
            "--banks",
            str(self.banks_dir),
            "--stub",
            str(self.stub),
            "--out",
            str(self.out),
        ]

        env = dict(os.environ)
        env["EXAM_MASTER"] = self.master_hex

        if self.keyfile:
            cmd += ["--emit-keyfile", str(self.keyfile)]

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
            )
        except OSError as e:
            self.done.emit(False, f"无法启动打包程序：{e}")
            return

        assert self._proc.stdout is not None
        for line in self._proc.stdout:
            # 安全兜底：万一某行带了 master（不应发生），打码后再显示
            self.log.emit(_redact(line.rstrip(), self.master_hex))

        code = self._proc.wait()
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
    def __init__(self, state: ProjectState) -> None:
        super().__init__()
        self.state = state
        self.worker: BuildWorker | None = None
        #: 本次打包用的临时目录，结束后必须删除（里面有题目原文）
        self._tmpdir: Path | None = None
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
        btn_gen = QPushButton("生成")
        btn_gen.setToolTip("调用 exam-builder keygen 生成一个随机主密钥")
        btn_gen.clicked.connect(self.gen_master)
        row.addWidget(btn_gen)
        form.addRow("主密钥", row)

        self.edit_stub = QLineEdit(self._default_stub())
        row2 = QHBoxLayout()
        row2.addWidget(self.edit_stub, 1)
        btn_stub = QPushButton("选择…")
        btn_stub.clicked.connect(lambda: self._pick_file(self.edit_stub, "选择学生端 stub"))
        row2.addWidget(btn_stub)
        form.addRow("学生端 stub", row2)

        self.edit_out = QLineEdit(str(self._default_out()))
        row3 = QHBoxLayout()
        row3.addWidget(self.edit_out, 1)
        btn_out = QPushButton("另存为…")
        btn_out.clicked.connect(self._pick_save)
        row3.addWidget(btn_out)
        form.addRow("输出包", row3)

        self.edit_keyfile = QLineEdit()
        self.edit_keyfile.setPlaceholderText("留空则不导出 K_manifest")
        row4 = QHBoxLayout()
        row4.addWidget(self.edit_keyfile, 1)
        btn_kf = QPushButton("选择…")
        btn_kf.clicked.connect(
            lambda: self._pick_file(self.edit_keyfile, "K_manifest 保存位置", save=True)
        )
        row4.addWidget(btn_kf)
        form.addRow("导出 K_manifest", row4)

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

    def gen_master(self) -> None:
        exe = builder_exe()
        if not exe.exists():
            QMessageBox.critical(
                self,
                "找不到 exam-builder",
                f"{exe}\n\n请先 cargo build --release -p builder",
            )
            return
        try:
            out = subprocess.run(
                [str(exe), "keygen"],
                capture_output=True,
                text=True,
                encoding="utf-8",  # 同 builder.py：Windows 默认 GBK 会解坏 UTF-8
                errors="replace",
                check=True,
            )
        except subprocess.CalledProcessError as e:
            QMessageBox.critical(self, "生成失败", str(e))
            return
        key = out.stdout.strip()
        # keygen 只输出 64 位 hex；若混入其他文本说明版本不对，直接提示
        if len(key) != 64 or any(c not in "0123456789abcdefABCDEF" for c in key):
            QMessageBox.critical(
                self, "生成失败", f"exam-builder 返回的内容不是 64 位十六进制：\n{key[:100]}"
            )
            return
        self.edit_master.setText(key)

    def refresh_preview(self) -> None:
        total = sum(len(s.students) for s in self.state.sessions)
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

    def start_build(self) -> None:
        if self.worker is not None and self.worker.isRunning():
            return

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
        out.parent.mkdir(parents=True, exist_ok=True)

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
        self._append(f"题库目录 {tmpdir}")
        self._append(f"清单 {manifest_path}")
        self.btn_build.setEnabled(False)
        self.btn_cancel.setEnabled(True)
        self.progress.setVisible(True)

        self.worker = BuildWorker(manifest_path, tmpdir, stub, out, master, keyfile)
        self.worker.log.connect(self._append)
        self.worker.done.connect(self._on_done)
        # 立刻清空界面上的 master：它已经进了 worker
        self.edit_master.clear()
        self.worker.start()

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
            self.worker.cancel()
            self._append("… 已请求取消")

    def _append(self, line: str) -> None:
        self.log_view.appendPlainText(line)

    def _on_done(self, ok: bool, info: str) -> None:
        self._cleanup_tmp()
        self.btn_build.setEnabled(True)
        self.btn_cancel.setEnabled(False)
        self.progress.setVisible(False)
        self.edit_master.clear()  # 无论成败都清

        if not ok:
            self._append(f"✗ 失败：{info}")
            QMessageBox.critical(self, "打包失败", info)
            return

        out = Path(info)
        self._append(f"✔ 成功：{out}")
        try:
            size = out.stat().st_size
            digest = hashlib.sha256(out.read_bytes()).hexdigest()
            self._append(f"  大小 {size:,} 字节")
            self._append(f"  SHA-256 {digest}")
        except OSError:
            pass
        QMessageBox.information(
            self,
            "打包成功",
            f"已生成：\n{out}\n\n"
            "✔ 密钥已内嵌在包内，学生双击即可使用（无需任何参数）。\n\n"
            "请核对日志里的 SHA-256。",
        )
