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

    dialog._consume_output("考试服务已启动：http://192.168.1.5:8080/\n")
    assert dialog.lbl_url.text() == access_url(dialog.spin_port.value())
    assert dialog.lbl_url.text().startswith("http://")


def test_url_uses_the_bound_address(qapp, tmp_path) -> None:
    """绑 127.0.0.1 时显示的地址也必须是 127.0.0.1（否则老师照着通知学生连不上）。"""
    dlg = _make_dialog(tmp_path)
    try:
        values = [dlg.combo_bind.itemData(i) for i in range(dlg.combo_bind.count())]
        assert "127.0.0.1" in values, "候选里应有「仅本机」"
        dlg.combo_bind.setCurrentIndex(values.index("127.0.0.1"))
        dlg._consume_output("考试服务已启动：http://127.0.0.1:8080/\n")
        assert dlg.lbl_url.text().startswith("http://127.0.0.1:"), dlg.lbl_url.text()
    finally:
        dlg.close()


def test_marker_split_across_two_reads_still_unlocks(qapp, tmp_path) -> None:
    """标志行被拆到两次输出里也要能识别（进程输出本来就不保证按行到达）。"""
    dlg = _make_dialog(tmp_path)
    try:
        dlg._consume_output("考试服务已启")
        assert "未启动" in dlg.lbl_url.text(), "半个标志不该放行"
        dlg._consume_output("动：http://192.168.1.5:8080/\n")
        assert dlg.lbl_url.text().startswith("http://"), "跨两次输出也要能拼出完整标志"
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
    fmt = '"考试服务已启动：http://{}:{}/"'
    assert fmt in src, "服务端就绪标志行变了，管理端的就绪判断要同步"
    assert fmt.strip('"').startswith(READY_MARKER), "READY_MARKER 必须是格式串的前缀"
    # GUI 传的两个参数必须仍是服务端的命令行参数（clap 由字段名生成 flag）。
    assert "time_check: clock::TimeMode" in src
    assert "login_rate_limit: u32" in src
