"""网页考试（exam-server）的纯逻辑：命令拼装与输入校验。

保持 Qt-free，方便单元测试；UI 在 `admin/app/ui/web_exam_dialog.py`。
"""

from __future__ import annotations

import socket
from pathlib import Path

#: 网页考试默认端口。
DEFAULT_PORT = 8080

#: 默认时间校验档位。`warn` = 取时失败只告警：机房 / 隔离局域网没有外网是常态，
#: 把它判成「不能开考」造成的停考事故远大于本机时钟偏差。
DEFAULT_TIME_CHECK = "warn"

#: 服务端 `--time-check` 允许的取值（与 `crates/server/src/clock.rs` 的 TimeMode 一致）。
TIME_CHECK_MODES = ("off", "warn", "require")

#: 默认「同 IP 每分钟登录尝试上限」。一个考场 50 人、每 IP 通常一两次，留足余量给
#: 「学生在同一个 NAT 后面」或老师复测。
DEFAULT_LOGIN_RATE_LIMIT = 60

#: 登录上限的界面可调范围（0 = 不限制）。
MAX_LOGIN_RATE_LIMIT = 10_000

#: 服务端「端口已绑定、可以接受连接」的标志行。
#: 必须与 `crates/server/src/main.rs` 的 `println!("考试服务已启动…")` 保持一致。
READY_MARKER = "考试服务已启动"


def web_command(
    *,
    server: Path,
    package: Path,
    keyfile: Path,
    port: int,
    answers_dir: Path,
    time_check: str = DEFAULT_TIME_CHECK,
    login_rate_limit: int = DEFAULT_LOGIN_RATE_LIMIT,
) -> list[str]:
    """拼出 `exam-server` 的 argv。

    服务端只需要考试包与 K_manifest；**master 与答案都不传入**。

    `--time-check` / `--login-rate-limit` 由教师在这里选（v1.4.6 起）：前者决定
    「没外网的机房照样开考」还是「时间不一致就拒绝启动」，后者决定同一 IP 每分钟
    允许多少次登录（0 = 不限，演练或学生都在同一个 NAT 后面时需要）。
    """
    return [
        str(server),
        "--package",
        str(package),
        "--keyfile",
        str(keyfile),
        "--port",
        str(port),
        "--answers-dir",
        str(answers_dir),
        "--time-check",
        time_check,
        "--login-rate-limit",
        str(login_rate_limit),
    ]


def validate_inputs(
    package: Path,
    keyfile: Path,
    port: int,
    *,
    time_check: str = DEFAULT_TIME_CHECK,
    login_rate_limit: int = DEFAULT_LOGIN_RATE_LIMIT,
) -> str | None:
    """返回错误文案；None 表示可以启动。

    错误文案要写清「发生了什么 + 怎么办」——这些提示会直接弹在教师面前。
    """
    if not package.is_file():
        return "请先选择一个已构建的考试包（学生端 exe）"
    if not keyfile.is_file():
        return "缺少 K_manifest：请在「打包」页设置「导出 K_manifest」并重新打包"
    if not (1024 <= port <= 65535):
        return "端口必须在 1024–65535 之间"
    if time_check not in TIME_CHECK_MODES:
        return f"时间校验必须是 {' / '.join(TIME_CHECK_MODES)} 之一（实际 {time_check!r}）"
    if not (0 <= login_rate_limit <= MAX_LOGIN_RATE_LIMIT):
        return (
            f"同 IP 登录上限必须在 0–{MAX_LOGIN_RATE_LIMIT} 之间（0 = 不限），"
            f"实际 {login_rate_limit}"
        )
    return None


def is_ready(output: str) -> bool:
    """服务端输出里出现「考试服务已启动」才算真的可用。

    为什么不能一启动就显示地址：v1.4.3 起服务端先做一次三源网络时间校验，无外网时
    最多要等约 6 秒，这期间进程已经起来、**端口还没绑定**。老师在进程启动那一刻
    把地址发给学生，学生看到的就是「连接被拒绝」。
    """
    return READY_MARKER in output


def port_available(port: int) -> bool:
    """检查端口能否绑定（避免启动后才发现被占用）。"""
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.bind(("0.0.0.0", port))
        return True
    except OSError:
        return False


def local_ip() -> str:
    """教师机在局域网里的地址（用于展示访问 URL）。"""
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
            s.connect(("8.8.8.8", 80))
            return s.getsockname()[0]
    except OSError:
        return "127.0.0.1"


def access_url(port: int, ip: str | None = None) -> str:
    return f"http://{ip or local_ip()}:{port}/"
