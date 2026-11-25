"""网页考试对话框：参数落地到命令行、地址等「服务就绪」再显示。

为什么单独测这个对话框：v1.4.6 起它能改「时间校验」与「同 IP 登录上限」两个
服务端参数——界面控件与命令行参数之间的映射一旦错位，老师看到的与实际生效的
就打不上，而这类错误在 GUI 上「看起来一切正常」。
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest
from PySide6.QtWidgets import QApplication

from app.core.webexam import access_url


@pytest.fixture(scope="module")
def qapp():
    app = QApplication.instance() or QApplication(sys.argv)
    yield app


def _make_dialog(tmp_path: Path):
    from app.ui.web_exam_dialog import WebExamDialog

    pkg = tmp_path / "pkg.exe"
    pkg.write_bytes(b"MZ")
    key = tmp_path / "key.txt"
    key.write_text("00" * 32, encoding="utf-8")
    return WebExamDialog(None, str(pkg), str(key))


@pytest.fixture
def dialog(qapp, tmp_path):
    dlg = _make_dialog(tmp_path)
    yield dlg
    dlg.close()


def _arg(args: list[str], flag: str) -> str:
    return args[args.index(flag) + 1]


def test_command_carries_time_check_and_rate_limit(dialog) -> None:
    args = dialog._command_args()
    assert _arg(args, "--time-check") == "warn", "默认应是 warn（机房无外网也能开考）"
    assert _arg(args, "--login-rate-limit") == "60", "默认每 IP 60 次/分钟"
    assert "--master" not in args, "网页考试不应把 master 传给服务端"


def test_changing_controls_changes_command(dialog) -> None:
    dialog.combo_time_check.setCurrentIndex(2)  # require
    dialog.spin_rate_limit.setValue(0)
    args = dialog._command_args()
    assert _arg(args, "--time-check") == "require"
    assert _arg(args, "--login-rate-limit") == "0"


def test_url_appears_only_after_service_is_ready(dialog) -> None:
    assert "未启动" in dialog.lbl_url.text()

    # 真实启动日志的前半段：先校时，再打印结论；此时端口还没绑定。
    dialog._consume_output("正在校验网络时间（warn 档…）…\n")
    dialog._consume_output("结论：正常（偏差 0 秒未超出容差）\n")
    assert "未启动" in dialog.lbl_url.text(), "端口还没绑定，不该显示地址"

    dialog._consume_output("考试服务已启动：https://192.168.1.5:8443/\n")
    assert dialog.lbl_url.text() == access_url(dialog.spin_port.value())
    assert dialog.lbl_url.text().startswith("https://")


def test_url_uses_the_bound_address(qapp, tmp_path) -> None:
    """绑 127.0.0.1 时显示的地址也必须是 127.0.0.1（否则老师照着通知学生连不上）。"""
    dlg = _make_dialog(tmp_path)
    try:
        values = [dlg.combo_bind.itemData(i) for i in range(dlg.combo_bind.count())]
        assert "127.0.0.1" in values, "候选里应有「仅本机」"
        dlg.combo_bind.setCurrentIndex(values.index("127.0.0.1"))
        dlg._consume_output("考试服务已启动：https://127.0.0.1:8443/\n")
        assert dlg.lbl_url.text().startswith("https://127.0.0.1:"), dlg.lbl_url.text()
    finally:
        dlg.close()


def test_marker_split_across_two_reads_still_unlocks(qapp, tmp_path) -> None:
    """标志行被拆到两次输出里也要能识别（进程输出本来就不保证按行到达）。"""
    dlg = _make_dialog(tmp_path)
    try:
        dlg._consume_output("考试服务已启")
        assert "未启动" in dlg.lbl_url.text(), "半个标志不该放行"
        dlg._consume_output("动：https://192.168.1.5:8443/\n")
        assert dlg.lbl_url.text().startswith("https://"), "跨两次输出也要能拼出完整标志"
    finally:
        dlg.close()


def test_controls_are_no_wheel(dialog) -> None:
    from app.ui.widgets import NoWheelComboBox, NoWheelSpinBox

    assert isinstance(dialog.combo_time_check, NoWheelComboBox), (
        "下拉框必须是 NoWheel（滚轮不切档）"
    )
    assert isinstance(dialog.spin_rate_limit, NoWheelSpinBox), "数字框必须是 NoWheel（滚轮不改值）"


def test_timeout_message_is_actionable(dialog) -> None:
    dialog._on_ready_timeout()
    assert "超时" in dialog.lbl_url.text()
    log = dialog.log.toPlainText()
    assert "时间校验" in log, "超时提示要给出下一步（改时间校验或换端口）：" + log


def test_off_mode_requires_confirmation(dialog, monkeypatch) -> None:
    """关闭时间校验是「危险方向」：必须二次确认，用户拒绝就不得启动。"""
    from PySide6.QtCore import QProcess
    from PySide6.QtWidgets import QMessageBox

    dialog.combo_time_check.setCurrentIndex(0)  # off
    asked: list[str] = []

    def _no(*args, **_kwargs):
        asked.append(str(args[1]) if len(args) > 1 else "")
        return QMessageBox.StandardButton.No

    monkeypatch.setattr(QMessageBox, "question", staticmethod(_no))
    dialog.start_server()

    assert asked, "选 off 档启动前必须先弹确认"
    assert "时间校验" in asked[0]
    assert dialog.proc.state() == QProcess.ProcessState.NotRunning, "用户拒绝后不得启动服务"


def test_confirmation_gate_only_applies_to_off(dialog, monkeypatch) -> None:
    """只有 off 需要确认：warn / require 直接放行，老师不该被无谓弹窗挡住。"""
    from PySide6.QtWidgets import QMessageBox

    asked: list[str] = []
    monkeypatch.setattr(
        QMessageBox,
        "question",
        staticmethod(lambda *a, **k: asked.append("q") or QMessageBox.StandardButton.Yes),
    )
    dialog.combo_time_check.setCurrentIndex(2)  # require
    assert dialog._confirm_time_check_off() is True
    dialog.combo_time_check.setCurrentIndex(1)  # warn
    assert dialog._confirm_time_check_off() is True
    assert not asked, "warn / require 不应弹确认"

    dialog.combo_time_check.setCurrentIndex(0)  # off
    assert dialog._confirm_time_check_off() is True, "用户点「是」时应放行"
    assert len(asked) == 1


def test_new_controls_are_laid_out_and_visible(dialog) -> None:
    """新增两只控件必须真的排进表单（不能挤进日志区或被裁掉）。

    不用截图判断布局（DPI/多显示器下结论不可靠）；这里直接问 Qt：
    控件是否可见、有没有非零尺寸、是否落在对话框内部。
    """
    dialog.show()
    QApplication.processEvents()
    rect = dialog.rect()
    for widget, name in (
        (dialog.chk_tls, "使用 HTTPS"),
        (dialog.combo_time_check, "时间校验"),
        (dialog.spin_rate_limit, "同 IP 登录上限"),
    ):
        assert widget.isVisible(), f"{name} 控件不可见"
        assert widget.width() > 0 and widget.height() > 0, f"{name} 控件尺寸为 0"
        top_left = widget.mapTo(dialog, widget.rect().topLeft())
        bottom_right = widget.mapTo(dialog, widget.rect().bottomRight())
        assert rect.contains(top_left) and rect.contains(bottom_right), (
            f"{name} 超出对话框范围：{top_left}..{bottom_right} vs {rect}"
        )


def test_ready_marker_matches_server_source() -> None:
    """GUI 靠「考试服务已启动」判断就绪；两边的契约写死在服务端源码里，必须对得上。

    这条是跨语言契约检查：服务端改了这句文案而这里没跟上，老师会一直看到
    「启动超时」——日志里明明已经起来了。
    """
    from app.core.webexam import READY_MARKER

    root = Path(__file__).resolve().parents[2]
    src = (root / "crates" / "server" / "src" / "main.rs").read_text(encoding="utf-8")
    # println! 可能被 rustfmt 折行，所以断言「格式串本身」而不是整行。
    fmt = '"考试服务已启动：{scheme}://{}:{}/"'
    assert fmt in src, "服务端就绪标志行变了，管理端的就绪判断要同步"
    assert fmt.strip('"').startswith(READY_MARKER), "READY_MARKER 必须是格式串的前缀"
    # GUI 传的两个参数必须仍是服务端的命令行参数（clap 由字段名生成 flag）。
    assert "time_check: clock::TimeMode" in src
    assert "login_rate_limit: u32" in src


def test_empty_answers_dir_falls_back_to_package_dir(qapp, tmp_path) -> None:
    """答题卡目录留空时必须回退到 `<考试包目录>/web-answers`，不能传空串。

    为什么：`--answers-dir ""` 会被 clap 判成「缺参数」直接退出（退出码 2），
    老师看到的是「启动超时」，而界面明明预填过值——清空一次就再也起不来。
    """
    dlg = _make_dialog(tmp_path)
    try:
        dlg.edit_answers.setText("")
        args = dlg._command_args()
        value = _arg(args, "--answers-dir")
        assert value, "留空时不能把空串传给服务端"
        assert value == str(tmp_path / "web-answers"), value
    finally:
        dlg.close()


# ---------------------------------------------------------------------------
# v1.5.1：HTTPS 勾选框（v1.5.0 的复审发现「界面控件根本没接线」）
# ---------------------------------------------------------------------------


def _force_check(checkbox, checked: bool) -> None:
    """改变勾选状态但不触发 `toggled`（绕开二次确认，模拟程序化赋值）。"""
    checkbox.blockSignals(True)
    checkbox.setChecked(checked)
    checkbox.blockSignals(False)


def test_tls_checkbox_defaults_to_https(dialog) -> None:
    """默认必须开 HTTPS：明文下同网段能抓到答案，不能靠老师记得勾。"""
    assert dialog.chk_tls.isChecked(), "默认应勾选「使用 HTTPS」"
    args = dialog._command_args()
    assert _arg(args, "--tls") == "auto"
    assert _arg(args, "--http-redirect-port") == "8080", "旧书签跳转端口要一起下发"


def test_tls_off_passes_explicit_flag_and_keeps_port(dialog) -> None:
    """关掉 HTTPS 必须显式传 `--tls off`（老版服务端会立刻报错，而不是静默跑明文）。"""
    _force_check(dialog.chk_tls, False)
    args = dialog._command_args()
    assert _arg(args, "--tls") == "off"
    assert _arg(args, "--port") == "8443", "关 TLS 不该顺带改端口"


def test_url_scheme_comes_from_server_banner(dialog, monkeypatch) -> None:
    """地址 scheme 取自服务端横幅，而不是「使用 HTTPS」勾选框。

    为什么：勾选框只决定**下一次启动**的 argv；运行中改它不会改变已经在跑的进程。
    v1.5.1 复审就抓到「运行中取消勾选 → 界面地址变成 http、进程仍在跑 TLS」，
    老师照此通知学生，全班连不上。
    """
    from PySide6.QtWidgets import QMessageBox

    monkeypatch.setattr(
        QMessageBox,
        "question",
        staticmethod(lambda *a, **k: QMessageBox.StandardButton.Yes),
    )
    dialog._consume_output("考试服务已启动：https://192.168.1.5:8443/\n")
    assert dialog.lbl_url.text().startswith("https://"), dialog.lbl_url.text()

    # 即使把勾选框改成「关闭 HTTPS」，地址也必须继续以横幅为准（不跟着变）。
    dialog.chk_tls.setChecked(False)
    assert dialog.lbl_url.text().startswith("https://"), (
        f"地址不能跟着勾选框变：{dialog.lbl_url.text()}"
    )


def test_url_scheme_follows_a_plaintext_banner(qapp, tmp_path) -> None:
    """横幅是 http 时地址就显示 http——哪怕勾选框还勾着（它只影响下次启动）。"""
    dlg = _make_dialog(tmp_path)
    try:
        assert dlg.chk_tls.isChecked(), "默认仍勾选 HTTPS（只决定下一次启动）"
        dlg._consume_output("考试服务已启动：http://192.168.1.5:8443/\n")
        assert dlg.lbl_url.text().startswith("http://"), dlg.lbl_url.text()
    finally:
        dlg.close()


def test_inputs_are_disabled_while_running(dialog, monkeypatch, tmp_path) -> None:
    """服务跑起来后，会改变进程行为的输入必须禁用，并提示「改动需重启服务生效」。"""
    from PySide6.QtWidgets import QMessageBox

    import app.ui.web_exam_dialog as mod

    monkeypatch.setattr(mod, "port_available", lambda _port: True)
    monkeypatch.setattr(QMessageBox, "warning", staticmethod(lambda *a, **k: None))
    server = tmp_path / "exam-server.exe"
    server.write_bytes(b"MZ")
    monkeypatch.setattr(dialog, "_server_path", lambda: server)
    started: list[tuple] = []
    monkeypatch.setattr(dialog.proc, "start", lambda *a: started.append(a))

    dialog.start_server()
    assert started, "start 应被调用"
    for widget, name in (
        (dialog.spin_port, "端口"),
        (dialog.chk_tls, "HTTPS"),
        (dialog.combo_bind, "监听地址"),
        (dialog.edit_answers, "答题卡目录"),
    ):
        assert not widget.isEnabled(), f"运行中「{name}」必须禁用（改了也不生效）"
    assert "重启" in dialog.lbl_locked.text(), dialog.lbl_locked.text()

    # 停止后恢复可编辑
    dialog._on_finished()
    assert dialog.spin_port.isEnabled() and dialog.chk_tls.isEnabled()
    assert dialog.lbl_locked.text() == ""


def test_declining_tls_off_keeps_https(dialog, monkeypatch) -> None:
    """用户拒绝关闭 HTTPS 时，勾选与地址都必须复原。"""
    from PySide6.QtWidgets import QMessageBox

    asked: list[str] = []
    monkeypatch.setattr(
        QMessageBox,
        "question",
        staticmethod(lambda *a, **k: asked.append("q") or QMessageBox.StandardButton.No),
    )
    dialog._consume_output("考试服务已启动：https://192.168.1.5:8443/\n")
    # 危险确认挂在 clicked（用户动作）：用 click() 复刻真实点击，而不是 setChecked。
    dialog.chk_tls.click()

    assert asked, "关闭 HTTPS 属危险方向，必须先确认"
    assert dialog.chk_tls.isChecked(), "拒绝后应保持 HTTPS"
    assert dialog.lbl_url.text().startswith("https://")


def test_programmatic_setchecked_never_prompts(dialog, monkeypatch) -> None:
    """程序化 setChecked(False) 不能弹「确认关闭 HTTPS」——AGENTS §7.6。

    确认必须挂在 `clicked`（用户动作）；若挂 `toggled`，初始化/恢复配置/测试都会弹窗。
    """
    from PySide6.QtWidgets import QMessageBox

    asked: list[str] = []
    monkeypatch.setattr(
        QMessageBox,
        "question",
        staticmethod(lambda *a, **k: asked.append("q") or QMessageBox.StandardButton.No),
    )
    dialog.chk_tls.setChecked(False)
    assert not asked, "程序化赋值不应触发危险确认"
    assert not dialog.chk_tls.isChecked()


def test_redirect_port_conflict_is_reported_not_blocking(dialog, monkeypatch, tmp_path) -> None:
    """8080 被占用只提示「旧书签跳转不可用」，**不拦启动**（主服务仍然可用）。

    旧版用例把 `_server_path` 指向不存在的文件，启动其实在「找不到服务端」就 return 了，
    等于只验证了「日志里有提示」；这里改成真实假文件 + 记录 `start` 调用。
    """
    from PySide6.QtWidgets import QMessageBox

    import app.ui.web_exam_dialog as mod

    main_port = dialog.spin_port.value()
    monkeypatch.setattr(mod, "port_available", lambda port: port == main_port)
    monkeypatch.setattr(QMessageBox, "warning", staticmethod(lambda *a, **k: None))
    server = tmp_path / "exam-server.exe"
    server.write_bytes(b"MZ")
    monkeypatch.setattr(dialog, "_server_path", lambda: server)
    started: list[tuple] = []
    monkeypatch.setattr(dialog.proc, "start", lambda *a: started.append(a))

    dialog.start_server()

    log = dialog.log.toPlainText()
    assert "旧书签" in log and "8080" in log, log
    assert started, "端口被占用只应提示，不能拦住启动"


def test_tls_checkbox_is_a_compact_checkbox(dialog) -> None:
    """勾选框必须走 `make_checkbox`（AGENTS §7.6：点说明文字不应切换状态）。"""
    from app.ui.widgets import CompactCheckBox

    assert isinstance(dialog.chk_tls, CompactCheckBox)


# ---------------------------------------------------------------------------
# v1.7.0：证书来源（自签 / 自己的证书）
# ---------------------------------------------------------------------------

_CERT_PEM = "-----BEGIN CERTIFICATE-----\nMIIBkTCB+wIJAO0example\n-----END CERTIFICATE-----\n"
# 同上：假 PEM 头拼接（gitleaks 会把私钥头的字面量当成真实私钥）。
_KEY_PEM = (
    "-----BEGIN " + "PRIVATE KEY-----\n"
    "MIGHAgEAMBMGByqGSM49example\n"
    "-----END " + "PRIVATE KEY-----\n"
)


def _select_source(dialog, value: str) -> None:
    combo = dialog.combo_tls_source
    index = combo.findData(value)
    assert index >= 0, f"证书来源里缺少 {value}"
    combo.setCurrentIndex(index)


def _make_cert_files(tmp_path: Path) -> tuple[Path, Path, Path]:
    base = tmp_path / "证书 目录"
    base.mkdir()
    cert = base / "服务 证书.crt"
    cert.write_text(_CERT_PEM, encoding="utf-8")
    key = base / "服务 私钥.key"
    key.write_text(_KEY_PEM, encoding="utf-8")
    ca = base / "内网 CA.pem"
    ca.write_text(_CERT_PEM, encoding="utf-8")
    return cert, key, ca


def _fake_file_dialog(monkeypatch, paths: list[Path]) -> None:
    """把三个「浏览…」依次指向给定路径（不弹真对话框）。"""
    from PySide6.QtWidgets import QFileDialog

    queue = list(paths)

    def _pick(*_args, **_kwargs):
        return (str(queue.pop(0)) if queue else "", "")

    monkeypatch.setattr(QFileDialog, "getOpenFileName", staticmethod(_pick))


def test_cert_source_defaults_to_self_signed_and_hides_fields(dialog) -> None:
    """默认自签：证书区域收起来，argv 里没有证书参数（与 v1.6.x 完全一致）。"""
    assert dialog.combo_tls_source.currentData() == "auto"
    assert dialog.cert_box.isHidden(), "自签时证书三行不该占版面"
    args = dialog._command_args()
    assert _arg(args, "--tls") == "auto", "自签模式仍要显式下发 --tls auto"
    for flag in ("--tls-cert", "--tls-key", "--tls-ca", "--tls-check"):
        assert flag not in args, f"{flag} 不该出现在自签模式的 argv 里"


def test_switching_to_custom_shows_fields(dialog) -> None:
    _select_source(dialog, "custom")
    assert not dialog.cert_box.isHidden(), "选「自己的证书」后三行必须显示"
    assert dialog.combo_tls_source.isEnabled()


def test_browse_buttons_fill_paths_and_reach_command(dialog, monkeypatch, tmp_path) -> None:
    """点「浏览…」选好文件后，四个参数必须真的进 argv（含空格与中文路径）。"""
    cert, key, ca = _make_cert_files(tmp_path)
    _fake_file_dialog(monkeypatch, [cert, key, ca])
    _select_source(dialog, "custom")

    dialog.btn_pick_cert.click()
    dialog.btn_pick_key.click()
    dialog.btn_pick_ca.click()

    assert dialog.edit_cert.text() == str(cert)
    assert dialog.edit_key.text() == str(key)
    assert dialog.edit_ca.text() == str(ca)

    args = dialog._command_args()
    assert _arg(args, "--tls-cert") == str(cert)
    assert _arg(args, "--tls-key") == str(key)
    assert _arg(args, "--tls-ca") == str(ca)
    assert _arg(args, "--tls-check") == "warn", "默认不严格（过期 / SAN 缺失只告警）"


def test_strict_checkbox_maps_to_strict_flag(dialog, monkeypatch, tmp_path) -> None:
    cert, key, _ca = _make_cert_files(tmp_path)
    _fake_file_dialog(monkeypatch, [cert, key])
    _select_source(dialog, "custom")
    dialog.btn_pick_cert.click()
    dialog.btn_pick_key.click()

    dialog.chk_tls_strict.setChecked(True)
    assert _arg(dialog._command_args(), "--tls-check") == "strict"


def test_switching_back_to_auto_clears_and_drops_flags(dialog, monkeypatch, tmp_path) -> None:
    """切回自签必须清空路径：残留路径绝不能再进 argv。"""
    cert, key, _ca = _make_cert_files(tmp_path)
    _fake_file_dialog(monkeypatch, [cert, key])
    _select_source(dialog, "custom")
    dialog.btn_pick_cert.click()
    dialog.btn_pick_key.click()
    dialog.chk_tls_strict.setChecked(True)
    assert "--tls-cert" in dialog._command_args()

    _select_source(dialog, "auto")

    assert dialog.cert_box.isHidden()
    assert dialog.edit_cert.text() == "" and dialog.edit_key.text() == ""
    assert not dialog.chk_tls_strict.isChecked(), "严格自检也要复位"
    args = dialog._command_args()
    for flag in ("--tls-cert", "--tls-key", "--tls-ca", "--tls-check"):
        assert flag not in args


def test_plaintext_disables_cert_group(dialog) -> None:
    """明文模式下证书没有意义：整组禁用（不隐藏，让老师看到它为什么不生效）。"""
    _select_source(dialog, "custom")
    _force_check(dialog.chk_tls, False)
    dialog._sync_cert_state()

    assert not dialog.combo_tls_source.isEnabled(), "明文下「证书来源」必须禁用"
    assert not dialog.cert_box.isEnabled(), "明文下证书区域必须禁用"
    assert _arg(dialog._command_args(), "--tls") == "off"


def test_cert_controls_disabled_while_running(dialog) -> None:
    """运行中所有会改变进程行为的输入都禁用，新增的证书控件也不例外。"""
    _select_source(dialog, "custom")
    dialog._set_inputs_enabled(False)
    assert not dialog.combo_tls_source.isEnabled()
    assert not dialog.cert_box.isEnabled()
    assert not dialog.edit_cert.isEnabled()

    dialog._set_inputs_enabled(True)
    assert dialog.combo_tls_source.isEnabled()
    assert dialog.cert_box.isEnabled()


def test_cert_area_fits_and_keeps_log_readable(dialog) -> None:
    """证书区域必须落在对话框内，且不能把日志区挤没。

    不用截图（AGENTS §4：DPI 下结论不可靠），直接问 Qt 几何：`cert_box` 的四个角
    都在 dialog 矩形内；切到 custom 后日志区仍留有可读高度（排查启动失败全靠它）。
    v1.7.0 加了三行文件选择，沿用 480 高度时日志只剩约 70 px。
    """
    _select_source(dialog, "custom")
    dialog.show()
    QApplication.processEvents()

    assert dialog.cert_box.isVisible()
    rect = dialog.rect()
    top_left = dialog.cert_box.mapTo(dialog, dialog.cert_box.rect().topLeft())
    bottom_right = dialog.cert_box.mapTo(dialog, dialog.cert_box.rect().bottomRight())
    assert rect.contains(top_left) and rect.contains(bottom_right), (
        f"证书区域超出对话框：{top_left}..{bottom_right} vs {rect}"
    )
    # 不比较绝对像素，也不比 `sizeHint()/minimumSizeHint()`：这两个值随字体与 DPI 变化，
    # Windows CI 上同一段代码量出来比本机高 50+ px（v1.7.0 就在这里假红过两次）。
    # 只断言三件稳定的事：① 证书区落在对话框内；② 日志区仍有可读高度（老师排查启动失败
    # 全靠它）；③ 窗口的**下界**已把「日志最小高度 + 其余内容」算进去（欠账会在现场被裁）。
    assert dialog.log.height() >= 100, f"日志区被压得太扁：{dialog.log.height()}px"
    assert dialog.minimumHeight() >= dialog.log.minimumHeight() + 200, (
        f"窗口下界没把日志区算进去：{dialog.minimumHeight()} < {dialog.log.minimumHeight()} + 200"
    )


def test_unchecking_https_through_real_click_disables_cert_group(
    dialog, monkeypatch, tmp_path
) -> None:
    """关掉 HTTPS 必须**经由真实点击**把证书组禁用（AGENTS §3：复刻真实接线）。

    旧用例是「手动调 `_sync_cert_state()` 摆出终态再断言」，等于只测了那个函数；
    真正出事的是接线——`_on_tls_clicked` 里那三行 `_sync_cert_state()` 被人删掉时，
    界面不会变灰、参数却悄悄不生效，而旧用例仍然全绿。
    """
    from PySide6.QtWidgets import QMessageBox

    cert, key, _ca = _make_cert_files(tmp_path)
    _fake_file_dialog(monkeypatch, [cert, key])
    _select_source(dialog, "custom")
    dialog.btn_pick_cert.click()
    dialog.btn_pick_key.click()
    assert "--tls-cert" in dialog._command_args()

    # 用户点勾选框 → 二次确认选「是」→ 接线应当自动禁用整组。
    monkeypatch.setattr(
        QMessageBox,
        "question",
        staticmethod(lambda *a, **k: QMessageBox.StandardButton.Yes),
    )
    dialog.chk_tls.click()

    assert not dialog.chk_tls.isChecked()
    assert not dialog.combo_tls_source.isEnabled(), "明文下「证书来源」必须随点击一起禁用"
    assert not dialog.cert_box.isEnabled(), "明文下证书区域必须随点击一起禁用"
    args = dialog._command_args()
    assert _arg(args, "--tls") == "off"
    for flag in ("--tls-cert", "--tls-key", "--tls-ca", "--tls-check"):
        assert flag not in args, f"{flag} 不该出现在明文模式的 argv 里"


def test_browse_cancel_keeps_fields_and_argv_clean(dialog, monkeypatch) -> None:
    """在文件对话框里点「取消」（返回空串）不能把路径写坏，也不能进 argv。

    `QFileDialog.getOpenFileName` 取消时返回 `("", "")`；如果实现里没判空，
    会把空串写进 line edit、`Path("")` 变成当前目录，`validate_inputs` 又会报
    「找不到证书文件：.」这种莫名其妙的错。
    """
    _fake_file_dialog(monkeypatch, [])  # 队列空 → 每次调用都返回 ("", "")
    _select_source(dialog, "custom")

    dialog.btn_pick_cert.click()
    dialog.btn_pick_key.click()
    dialog.btn_pick_ca.click()

    assert dialog.edit_cert.text() == ""
    assert dialog.edit_key.text() == ""
    assert dialog.edit_ca.text() == ""
    args = dialog._command_args()
    for flag in ("--tls-cert", "--tls-key", "--tls-ca"):
        assert flag not in args, f"取消选择后 {flag} 不该出现"


def test_failed_start_reports_exit_code_instead_of_stopped(dialog) -> None:
    """服务端没起来就退出（最常见：外部证书自检失败）时，状态行要说「启动失败」。

    只写「已停止」会让老师以为是自己点错了按钮，而真正原因在日志里的 `✗ …` 那几行。
    """
    dialog._ready = False
    # 真的拉起进程再等它退出太慢；直接把 `exitCode()` 换成固定值即可。
    dialog.proc.exitCode = lambda: 1  # type: ignore[method-assign]
    dialog._on_finished()
    assert "启动失败" in dialog.lbl_url.text(), dialog.lbl_url.text()
    assert "1" in dialog.lbl_url.text()

    # 正常停掉（曾经 ready 过）仍显示「已停止」，不要误报失败。
    dialog._ready = True
    dialog._on_finished()
    assert dialog.lbl_url.text() == "已停止"


def test_roster_file_is_written_and_passed(qapp, tmp_path) -> None:
    """项目学号要写成 `--roster` 能读的格式（去重 + 升序），并出现在 argv 里。"""
    dlg = _make_dialog(tmp_path)
    try:
        dlg._roster_ids = [2021003, 2021001, 2021001]
        path = dlg._write_roster()
        assert path is not None
        lines = [
            ln for ln in path.read_text(encoding="utf-8").splitlines() if not ln.startswith("#")
        ]
        assert lines == ["2021001", "2021003"], lines
        args = dlg._command_args()
        assert _arg(args, "--roster") == str(path)
    finally:
        dlg.close()


def test_no_roster_means_no_flag(qapp, tmp_path) -> None:
    """没有学号时不能传 `--roster`（否则服务端会去读一个不存在的文件）。"""
    dlg = _make_dialog(tmp_path)
    try:
        dlg._roster_ids = []
        assert dlg._write_roster() is None
        assert "--roster" not in dlg._command_args()
    finally:
        dlg.close()
