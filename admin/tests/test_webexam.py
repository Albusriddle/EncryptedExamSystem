"""网页考试命令拼装与校验。"""

from pathlib import Path

from app.core.webexam import (
    DEFAULT_LOGIN_RATE_LIMIT,
    DEFAULT_TIME_CHECK,
    access_url,
    is_ready,
    validate_inputs,
    web_command,
)


def test_web_command_passes_package_and_keyfile_only(tmp_path: Path) -> None:
    cmd = web_command(
        server=Path("exam-server.exe"),
        package=tmp_path / "pkg.exe",
        keyfile=tmp_path / "key.txt",
        port=8080,
        answers_dir=tmp_path / "answers",
    )
    assert cmd[0].endswith("exam-server.exe")
    assert "--package" in cmd and "--keyfile" in cmd
    assert "--master" not in cmd, "网页考试不应把 master 传给服务端"
    assert cmd[cmd.index("--port") + 1] == "8080"


def test_web_command_carries_time_check_and_rate_limit(tmp_path: Path) -> None:
    """v1.4.6：两个服务端参数由管理端界面控制，必须真的进 argv。"""
    cmd = web_command(
        server=Path("exam-server.exe"),
        package=tmp_path / "pkg.exe",
        keyfile=tmp_path / "key.txt",
        port=8080,
        answers_dir=tmp_path / "answers",
    )
    assert cmd[cmd.index("--time-check") + 1] == DEFAULT_TIME_CHECK == "warn"
    assert cmd[cmd.index("--login-rate-limit") + 1] == str(DEFAULT_LOGIN_RATE_LIMIT) == "60"

    cmd = web_command(
        server=Path("exam-server.exe"),
        package=tmp_path / "pkg.exe",
        keyfile=tmp_path / "key.txt",
        port=8080,
        answers_dir=tmp_path / "answers",
        time_check="require",
        login_rate_limit=0,
    )
    assert cmd[cmd.index("--time-check") + 1] == "require"
    assert cmd[cmd.index("--login-rate-limit") + 1] == "0"


def test_validate_inputs(tmp_path: Path) -> None:
    pkg = tmp_path / "pkg.exe"
    key = tmp_path / "key.txt"
    assert validate_inputs(pkg, key, 8080) is not None
    pkg.write_bytes(b"MZ")
    assert validate_inputs(pkg, key, 8080) is not None
    key.write_text("00" * 32, encoding="utf-8")
    assert validate_inputs(pkg, key, 80) is not None
    assert validate_inputs(pkg, key, 8080) is None


def test_validate_inputs_rejects_bad_new_params(tmp_path: Path) -> None:
    pkg = tmp_path / "pkg.exe"
    key = tmp_path / "key.txt"
    pkg.write_bytes(b"MZ")
    key.write_text("00" * 32, encoding="utf-8")

    bad_mode = validate_inputs(pkg, key, 8080, time_check="strict")
    assert bad_mode is not None and "时间校验" in bad_mode

    bad_limit = validate_inputs(pkg, key, 8080, login_rate_limit=-1)
    assert bad_limit is not None and "登录上限" in bad_limit
    assert validate_inputs(pkg, key, 8080, login_rate_limit=10_001) is not None

    # 边界值都应放行：0 = 不限；require 是合法档位。
    assert validate_inputs(pkg, key, 8080, time_check="require", login_rate_limit=0) is None


def test_is_ready_needs_the_full_marker() -> None:
    """只有服务端打印出「考试服务已启动」才算就绪——半个标志不能放行。"""
    assert not is_ready("正在校验网络时间（warn 档…）…\n结论：正常\n")
    assert not is_ready("考试服务已启")
    assert is_ready("考试服务已启动：http://192.168.1.5:8080/\n")
    assert is_ready("多行\n考试服务已启动\n")


def test_access_url_uses_given_ip() -> None:
    assert access_url(8080, "192.168.1.5") == "http://192.168.1.5:8080/"
