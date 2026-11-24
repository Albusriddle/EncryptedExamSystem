"""网页考试：启动/停止本地 exam-server，并展示访问地址。"""

from __future__ import annotations

from pathlib import Path

from PySide6.QtCore import QProcess, QTimer
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

from app.core.webexam import (
    DEFAULT_BIND,
    DEFAULT_HTTP_REDIRECT_PORT,
    DEFAULT_LOGIN_RATE_LIMIT,
    DEFAULT_PORT,
    DEFAULT_TIME_CHECK,
    MAX_LOGIN_RATE_LIMIT,
    TIME_CHECK_MODES,
    access_url,
    advertised_host,
    bind_choices,
    is_ready,
    port_available,
    validate_inputs,
    web_command,
)
from app.ui.widgets import NoWheelComboBox, NoWheelSpinBox, fit_button_text, make_checkbox

#: 就绪等待上限：默认档要等一次三源取时（无外网时约 6 秒），留足余量。
READY_TIMEOUT_MS = 30_000

_TIME_CHECK_LABELS = {
    "off": "off：不校验（完全依赖本机时钟）",
    "warn": "warn：只告警（默认，机房无外网也能开考）",
    "require": "require：不通过就拒绝启动",
}


class WebExamDialog(QDialog):
    """非模态对话框：考试期间可一直开着，关闭时自动停服务。"""

    def __init__(self, parent, package: str, keyfile: str) -> None:
        super().__init__(parent)
        self.setWindowTitle("网页考试（局域网）")
        self.resize(680, 480)
        self.proc = QProcess(self)
        self.proc.setProcessChannelMode(QProcess.ProcessChannelMode.MergedChannels)
        self.proc.readyReadStandardOutput.connect(self._read_output)
        self.proc.finished.connect(self._on_finished)
        self.proc.started.connect(self._on_started)
        self.proc.errorOccurred.connect(self._on_error)

        # 就绪状态：只有看到服务端打印「考试服务已启动」才显示访问地址。
        self._output_buffer = ""
        self._ready = False
        self._ready_timer = QTimer(self)
        self._ready_timer.setSingleShot(True)
        self._ready_timer.timeout.connect(self._on_ready_timeout)

        form = QFormLayout()
        self.edit_package = QLineEdit(package)
        self.edit_keyfile = QLineEdit(keyfile)
        self.spin_port = NoWheelSpinBox()
        self.spin_port.setRange(1024, 65535)
        self.spin_port.setValue(DEFAULT_PORT)

        # v1.5.0 起服务端默认 HTTPS（自签证书）。关闭它是**危险方向**：明文 HTTP 下
        # 同网段任何人都能抓到学生作答、PIN 与会话 cookie，所以取消勾选要二次确认。
        self.chk_tls = make_checkbox("使用 HTTPS（自签证书，推荐）")
        self.chk_tls.setChecked(True)
        self.chk_tls.setToolTip(
            "默认开启：服务端自动生成自签证书，学生首次打开会提示「不安全」，"
            "点「高级 → 继续前往」即可。\n"
            "iPad / iPhone 上的 Chrome 无法绕过证书警告，请改用 Safari。\n"
            "教师屏会显示证书指纹，学生可在浏览器证书详情里核对（防中间人）。\n"
            "关闭后走明文 HTTP——同网段可嗅探答案，仅建议排障时使用。"
        )
        self.chk_tls.toggled.connect(self._on_tls_toggled)
        self.edit_answers = QLineEdit(str(Path(package).parent / "web-answers") if package else "")

        # 监听地址：默认所有网卡；多网卡教师机只绑 LAN 地址时，外网侧根本连不上。
        self.combo_bind = NoWheelComboBox()
        for value, label in bind_choices():
            self.combo_bind.addItem(label, value)
        self.combo_bind.setToolTip(
            "服务端监听哪块网卡。默认「全部网卡」（0.0.0.0）。\n"
            "教师机同时连着校园网时，选「仅 <本机局域网地址>」可以让学生只在机房侧访问；\n"
            "「仅本机」只给自己在本机打开浏览器调试用。"
        )

        # 时间校验档位：默认 warn。选 require 意味着「取时失败就不让开考」，
        # 在隔离局域网里会直接停考，所以文案里把后果写清楚。
        self.combo_time_check = NoWheelComboBox()
        for mode in TIME_CHECK_MODES:
            self.combo_time_check.addItem(_TIME_CHECK_LABELS[mode], mode)
        self.combo_time_check.setCurrentIndex(TIME_CHECK_MODES.index(DEFAULT_TIME_CHECK))
        self.combo_time_check.setToolTip(
            "启动时用三个公共站点核对网络时间。\n"
            "warn（默认）：取时失败只写日志，照常开考；\n"
            "require：取时失败或本机时钟偏差过大就不启动（适合有外网的考场）；\n"
            "off：完全不校验（不推荐，场次判定将完全依赖本机时钟）。"
        )

        # 同 IP 登录上限：0 = 不限。
        self.spin_rate_limit = NoWheelSpinBox()
        self.spin_rate_limit.setRange(0, MAX_LOGIN_RATE_LIMIT)
        self.spin_rate_limit.setValue(DEFAULT_LOGIN_RATE_LIMIT)
        self.spin_rate_limit.setSuffix(" 次/分钟（0 = 不限）")
        self.spin_rate_limit.setToolTip(
            "同一 IP 每分钟允许的登录尝试次数，默认 60。\n"
            "学生各自一个 IP 时通常用不到；若整班在同一个 NAT 后面、或做并发演练，"
            "把它调大或设成 0。"
        )

        form.addRow("考试包", self.edit_package)
        form.addRow("K_manifest", self.edit_keyfile)
        form.addRow("端口", self.spin_port)
        form.addRow("HTTPS", self.chk_tls)
        form.addRow("监听地址", self.combo_bind)
        form.addRow("时间校验", self.combo_time_check)
        form.addRow("同 IP 登录上限", self.spin_rate_limit)
        form.addRow("答题卡目录", self.edit_answers)
        # 让老师知道证书落在哪、以及为什么学生会看到一次警告页——否则第一节课
        # 会被「浏览器说不安全」的举手打断。
        form.addRow(
            "证书",
            QLabel("由服务端自动生成到「答题卡目录/tls/」下（首次访问点「高级 → 继续前往」）"),
        )

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
        lay.addWidget(QLabel("学生访问地址（服务就绪后显示）："))
        lay.addWidget(self.lbl_url)
        lay.addLayout(row)
        lay.addWidget(self.log, 1)
        lay.addWidget(
            QLabel(
                "默认 HTTPS（自签证书）：学生首次访问点「高级 → 继续前往」；iPad/iPhone 用 Safari；"
                "关闭 HTTPS 为明文（同网段可嗅探），仅建议排障时使用。"
            )
        )

    # -- 参数 -------------------------------------------------------------

    def _time_check(self) -> str:
        return str(self.combo_time_check.currentData())

    def _confirm_time_check_off(self) -> bool:
        """`off` 档要二次确认；其它档位直接放行。

        抽成方法是为了能单独测「危险方向必须确认」这条规则，
        不必真的把服务拉起来。
        """
        if self._time_check() != "off":
            return True
        answer = QMessageBox.question(
            self,
            "确认关闭时间校验？",
            "关闭后场次判定将完全依赖教师机系统时钟：时钟设错就会判错开考 / 收卷，"
            "而且启动日志里不会再有网络时间提示。\n\n确定用 off 档启动吗？",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
            QMessageBox.StandardButton.No,
        )
        return answer == QMessageBox.StandardButton.Yes

    def _bind(self) -> str:
        value = str(self.combo_bind.currentData() or DEFAULT_BIND)
        return value

    def _login_rate_limit(self) -> int:
        return int(self.spin_rate_limit.value())

    def _answers_dir(self) -> Path:
        """答题卡目录：留空时回退到 `<考试包目录>/web-answers`。

        为什么不能直接把空串传给服务端：`--answers-dir ""` 会被 clap 当成「缺参数」
        直接退出（退出码 2），老师看到的是「启动超时」，而界面明明预填过值——
        清空一次就再也起不来。界面预填值、建目录、传参必须用**同一个**回退逻辑。
        """
        raw = self.edit_answers.text().strip()
        if raw:
            return Path(raw)
        package = Path(self.edit_package.text().strip())
        return package.parent / "web-answers"

    def _command_args(self) -> list[str]:
        """按当前界面值拼出 argv（不含程序路径以外的改动，便于测试）。

        抽成方法是为了让测试能直接断言「界面 → 命令行」的映射，
        不必真的把服务拉起来。
        """
        return web_command(
            server=self._server_path(),
            package=Path(self.edit_package.text().strip()),
            keyfile=Path(self.edit_keyfile.text().strip()),
            port=self.spin_port.value(),
            answers_dir=self._answers_dir(),
            bind=self._bind(),
            time_check=self._time_check(),
            login_rate_limit=self._login_rate_limit(),
            # TLS 模式必须显式下发：老版 exam-server 不认这个参数会立刻报错，
            # 而不是「界面显示 https、实际跑 http」这种没人能发现的静默错配。
            tls="auto" if self.chk_tls.isChecked() else "off",
        )

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

    # -- 生命周期 ---------------------------------------------------------

    def start_server(self) -> None:
        package = Path(self.edit_package.text().strip())
        keyfile = Path(self.edit_keyfile.text().strip())
        err = validate_inputs(
            package,
            keyfile,
            self.spin_port.value(),
            bind=self._bind(),
            time_check=self._time_check(),
            login_rate_limit=self._login_rate_limit(),
            tls="auto" if self.chk_tls.isChecked() else "off",
        )
        if err:
            QMessageBox.warning(self, "无法启动", err)
            return
        if not port_available(self.spin_port.value()):
            QMessageBox.warning(
                self, "端口被占用", f"端口 {self.spin_port.value()} 已被其他程序占用"
            )
            return
        # 旧书签（http://IP:8080/）靠服务端另起的明文端口做 301 跳转。它被占用时**不拦启动**
        # （主服务照常可用），但必须写进日志：否则老师会以为旧链接还能用，学生打开却是报错。
        if (
            self.chk_tls.isChecked()
            and self.spin_port.value() != DEFAULT_HTTP_REDIRECT_PORT
            and not port_available(DEFAULT_HTTP_REDIRECT_PORT)
        ):
            self.log.appendPlainText(
                f"⚠ 明文跳转端口 {DEFAULT_HTTP_REDIRECT_PORT} 被占用：旧书签 "
                f"http://…:{DEFAULT_HTTP_REDIRECT_PORT}/ 不会自动跳到 HTTPS，"
                "请把新地址（https）发给学生。"
            )
        # 关掉时间校验是「危险方向」：场次判定会完全依赖教师机时钟，而且不再有任何提示。
        # 按项目约定，危险方向的切换要二次确认，且确认挂在用户动作（这里是点「启动」）上。
        if not self._confirm_time_check_off():
            return
        server = self._server_path()
        if not server.is_file():
            QMessageBox.warning(self, "找不到服务端", f"缺少 exam-server：{server}")
            return
        # 建目录与 `--answers-dir` 必须用同一个回退值，否则「留空」时目录建了、
        # 服务端却因为空参数起不来（见 `_answers_dir`）。
        answers = self._answers_dir()
        answers.mkdir(parents=True, exist_ok=True)

        self._output_buffer = ""
        self._ready = False
        self.proc.start(str(server), self._command_args()[1:])
        self.btn_start.setEnabled(False)
        self.btn_stop.setEnabled(True)
        self.lbl_url.setText("正在启动…（等服务就绪）")
        self._ready_timer.start(READY_TIMEOUT_MS)

    def stop_server(self) -> None:
        self._ready_timer.stop()
        if self.proc.state() != QProcess.ProcessState.NotRunning:
            self.proc.terminate()
            if not self.proc.waitForFinished(5000):
                self.proc.kill()

    # -- 事件 -------------------------------------------------------------

    def _on_started(self) -> None:
        # 进程起来了，但端口还没绑定：地址等「考试服务已启动」再显示。
        self.lbl_url.setText("正在启动…（等服务就绪）")

    def _on_error(self, _err) -> None:
        self._ready_timer.stop()
        self.lbl_url.setText("启动失败，见下方日志")
        self.btn_start.setEnabled(True)
        self.btn_stop.setEnabled(False)

    def _on_finished(self) -> None:
        self._ready_timer.stop()
        self._ready = False
        self.btn_start.setEnabled(True)
        self.btn_stop.setEnabled(False)
        self.lbl_url.setText("已停止")

    def _on_ready_timeout(self) -> None:
        if self._ready:
            return
        self.lbl_url.setText("启动超时：见下方日志")
        self.log.appendPlainText(
            "⚠ 30 秒内没有等到「考试服务已启动」。若日志里是时间校验失败，"
            "把「时间校验」调成 warn 或 off 再试；若是端口占用，换一个端口。"
        )

    def _read_output(self) -> None:
        data = bytes(self.proc.readAllStandardOutput()).decode("utf-8", "replace")
        if data:
            self._consume_output(data)

    def _consume_output(self, text: str) -> None:
        """把服务端输出喂进来：写日志 + 检测就绪。

        抽成独立方法是为了让测试不必真的拉起进程：直接喂一段真实日志即可验证
        「就绪前不显示地址、就绪后显示地址」。
        """
        if not text:
            return
        self.log.appendPlainText(text.rstrip())
        self._output_buffer += text
        if not self._ready and is_ready(self._output_buffer):
            self._ready = True
            self._ready_timer.stop()
            self._refresh_url()

    def _refresh_url(self) -> None:
        """按当前勾选框与端口刷新地址；未就绪时不显示（端口还没绑）。

        地址的 scheme 必须跟着「使用 HTTPS」走：否则关掉 TLS 后界面仍显示 https，
        老师照着发给学生，全班连不上。
        """
        if not self._ready:
            return
        self.lbl_url.setText(
            access_url(
                self.spin_port.value(),
                advertised_host(self._bind()),
                tls=self.chk_tls.isChecked(),
            )
        )

    def _on_tls_toggled(self, checked: bool) -> None:
        """关闭 HTTPS 是危险方向：明文下同网段可抓到答案、PIN 与 cookie。

        确认放在 `toggled`（由用户的点击/键盘触发）而不是 `clicked`，是因为
        `CompactCheckBox` 的点击区会延伸到说明文字上，`toggled` 更稳；程序化赋值
        （测试/初始化）前会 `blockSignals`，不会误弹窗。
        """
        if checked:
            self._refresh_url()
            return
        answer = QMessageBox.question(
            self,
            "确认关闭 HTTPS？",
            "关闭后学生与教师机之间是**明文 HTTP**：同一局域网内任何人都能抓到学生的"
            "作答、PIN 与会话 cookie。\n\n只应在排障（或设备无法绕过证书警告）时临时关闭。\n"
            "确定关闭 HTTPS 吗？",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
            QMessageBox.StandardButton.No,
        )
        if answer == QMessageBox.StandardButton.Yes:
            self._refresh_url()
            return
        # 用户拒绝：把勾复原。blockSignals 防「setChecked 又触发 toggled」的递归弹窗。
        self.chk_tls.blockSignals(True)
        self.chk_tls.setChecked(True)
        self.chk_tls.blockSignals(False)

    def _copy_url(self) -> None:
        from PySide6.QtWidgets import QApplication

        QApplication.clipboard().setText(self.lbl_url.text())

    def closeEvent(self, event) -> None:  # noqa: N802
        self._ready_timer.stop()
        self.stop_server()
        super().closeEvent(event)
