"""网页考试：启动/停止本地 exam-server，并展示访问地址。"""

from __future__ import annotations

from pathlib import Path

from PySide6.QtCore import QProcess
from PySide6.QtWidgets import (
    QDialog,
    QFormLayout,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QMessageBox,
    QPlainTextEdit,
    QPushButton,
    QVBoxLayout,
)

from app.core.webexam import DEFAULT_PORT, access_url, validate_inputs, web_command
from app.ui.widgets import NoWheelSpinBox, fit_button_text


class WebExamDialog(QDialog):
    """非模态对话框：考试期间可一直开着，关闭时自动停服务。"""

    def __init__(self, parent, package: str, keyfile: str) -> None:
        super().__init__(parent)
        self.setWindowTitle("网页考试（局域网）")
        self.resize(640, 420)
        self.proc = QProcess(self)
        self.proc.setProcessChannelMode(QProcess.ProcessChannelMode.MergedChannels)
        self.proc.readyReadStandardOutput.connect(self._read_output)
        self.proc.finished.connect(self._on_finished)

        form = QFormLayout()
        self.edit_package = QLineEdit(package)
        self.edit_keyfile = QLineEdit(keyfile)
        self.spin_port = NoWheelSpinBox()
        self.spin_port.setRange(1024, 65535)
        self.spin_port.setValue(DEFAULT_PORT)
        self.edit_answers = QLineEdit(str(Path(package).parent / "web-answers") if package else "")
        form.addRow("考试包", self.edit_package)
        form.addRow("K_manifest", self.edit_keyfile)
        form.addRow("端口", self.spin_port)
        form.addRow("答题卡目录", self.edit_answers)

        self.lbl_url = QLabel("未启动")
        self.lbl_url.setTextInteractionFlags(self.lbl_url.textInteractionFlags())
        self.btn_start = QPushButton("启动服务")
        self.btn_stop = QPushButton("停止")
        self.btn_stop.setEnabled(False)
        self.btn_copy = QPushButton("复制地址")
        self.btn_start.clicked.connect(self.start_server)
        self.btn_stop.clicked.connect(self.stop_server)
        self.btn_copy.clicked.connect(self._copy_url)
        row = QHBoxLayout()
        row.addWidget(self.btn_start)
        row.addWidget(self.btn_stop)
        row.addWidget(self.btn_copy)
        for btn in (self.btn_start, self.btn_stop, self.btn_copy):
            fit_button_text(btn)
        row.addStretch(1)

        self.log = QPlainTextEdit()
        self.log.setReadOnly(True)
        self.log.setPlaceholderText("服务输出…")

        lay = QVBoxLayout(self)
        lay.addLayout(form)
        lay.addWidget(QLabel("学生访问地址："))
        lay.addWidget(self.lbl_url)
        lay.addLayout(row)
        lay.addWidget(self.log, 1)
        lay.addWidget(QLabel("仅用于隔离局域网 + 现场监考；HTTP 明文，勿接入互联网。"))

    def _server_path(self) -> Path:
        import sys

        cands = []
        if getattr(sys, "_MEIPASS", None):
            cands.append(Path(sys._MEIPASS) / "exam-server.exe")
        root = Path(__file__).resolve().parents[3]
        cands += [
            root / "target" / "release" / "exam-server.exe",
            root / "target" / "debug" / "exam-server.exe",
        ]
        for c in cands:
            if c.is_file():
                return c
        return cands[0]

    def start_server(self) -> None:
        package = Path(self.edit_package.text().strip())
        keyfile = Path(self.edit_keyfile.text().strip())
        err = validate_inputs(package, keyfile, self.spin_port.value())
        if err:
            QMessageBox.warning(self, "无法启动", err)
            return
        server = self._server_path()
        if not server.is_file():
            QMessageBox.warning(self, "找不到服务端", f"缺少 exam-server：{server}")
            return
        answers = Path(self.edit_answers.text().strip() or (package.parent / "web-answers"))
        answers.mkdir(parents=True, exist_ok=True)
        self.proc.start(
            str(server),
            web_command(
                server=server,
                package=package,
                keyfile=keyfile,
                port=self.spin_port.value(),
                answers_dir=answers,
            )[1:],
        )
        self.btn_start.setEnabled(False)
        self.btn_stop.setEnabled(True)
        self.lbl_url.setText(access_url(self.spin_port.value()))

    def stop_server(self) -> None:
        if self.proc.state() != QProcess.ProcessState.NotRunning:
            self.proc.terminate()
            if not self.proc.waitForFinished(5000):
                self.proc.kill()

    def _on_finished(self) -> None:
        self.btn_start.setEnabled(True)
        self.btn_stop.setEnabled(False)
        self.lbl_url.setText("已停止")

    def _read_output(self) -> None:
        data = bytes(self.proc.readAllStandardOutput()).decode("utf-8", "replace")
        self.log.appendPlainText(data.rstrip())

    def _copy_url(self) -> None:
        from PySide6.QtWidgets import QApplication

        QApplication.clipboard().setText(self.lbl_url.text())

    def closeEvent(self, event) -> None:  # noqa: N802
        self.stop_server()
        super().closeEvent(event)
