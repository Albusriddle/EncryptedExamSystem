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

from app.core.state import ProjectState


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

    # ---------------------------------------------------------------- 默认值

    def _default_stub(self) -> str:
        root = Path(__file__).resolve().parents[3]
        name = "exam-client.exe" if os.name == "nt" else "exam-client"
        return str(root / "target" / "release" / name)

    def _default_out(self) -> Path:
        root = Path(__file__).resolve().parents[3]
        return root / "dist" / "StudentClient.exe"

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
            QMessageBox.critical(self, "找不到 stub", str(stub))
            return

        out = Path(self.edit_out.text())
        out.parent.mkdir(parents=True, exist_ok=True)

        # 题库目录：取所有题库路径的公共父目录太多变，改用临时目录扁平复制
        tmpdir = Path(tempfile.mkdtemp(prefix="examadm-"))
        try:
            for exam_id, bank in self.state.banks.items():
                (tmpdir / f"{exam_id}.json").write_text(
                    bank.model_dump_json(by_alias=True), encoding="utf-8"
                )
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

    def cancel_build(self) -> None:
        if self.worker:
            self.worker.cancel()
            self._append("… 已请求取消")

    def _append(self, line: str) -> None:
        self.log_view.appendPlainText(line)

    def _on_done(self, ok: bool, info: str) -> None:
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
