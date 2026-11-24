"""网页考试命令拼装与校验。"""

from pathlib import Path

from app.core.webexam import (
    DEFAULT_BIND,
    DEFAULT_HTTP_REDIRECT_PORT,
    DEFAULT_LOGIN_RATE_LIMIT,
    DEFAULT_TIME_CHECK,
    access_url,
    bind_choices,
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


def test_web_command_carries_bind(tmp_path: Path) -> None:
    common = dict(
        server=Path("exam-server.exe"),
        package=tmp_path / "pkg.exe",
        keyfile=tmp_path / "key.txt",
        port=8080,
        answers_dir=tmp_path / "answers",
    )
    cmd = web_command(**common)
    assert cmd[cmd.index("--bind") + 1] == DEFAULT_BIND == "0.0.0.0"

    cmd = web_command(**common, bind="192.168.1.5")
    assert cmd[cmd.index("--bind") + 1] == "192.168.1.5"


def test_validate_inputs_rejects_bad_bind(tmp_path: Path) -> None:
    pkg = tmp_path / "pkg.exe"
    key = tmp_path / "key.txt"
    pkg.write_bytes(b"MZ")
    key.write_text("00" * 32, encoding="utf-8")

    for bad in ("", "localhost", "192.168.1.999", "0.0.0.0:8080"):
        msg = validate_inputs(pkg, key, 8080, bind=bad)
        assert msg is not None, f"{bad!r} 应被拒绝"
        assert "监听地址" in msg

    assert validate_inputs(pkg, key, 8080, bind="0.0.0.0") is None
    assert validate_inputs(pkg, key, 8080, bind="127.0.0.1") is None
    assert validate_inputs(pkg, key, 8080, bind="::1") is None, "IPv6 也应允许"


def test_bind_choices_are_sane() -> None:
    choices = bind_choices()
    values = [value for value, _label in choices]
    assert values[0] == "0.0.0.0", "默认必须是全部网卡，排第一"
    assert values[-1] == "127.0.0.1", "本机调试项排最后"
    assert len(values) == len(set(values)), "候选不能重复"
    assert all(label for _value, label in choices), "每项都要有人能读的标签"


def test_advertised_host_follows_bind() -> None:
    from app.core.webexam import advertised_host, local_ip

    assert advertised_host("0.0.0.0") == local_ip(), "通配地址要换成局域网 IP"
    assert advertised_host("::") == local_ip()
    assert advertised_host("127.0.0.1") == "127.0.0.1", "绑回环就显示回环"
    assert advertised_host("192.168.1.5") == "192.168.1.5"


def test_access_url_uses_given_ip() -> None:
    # v1.5.0：默认 HTTPS（自签证书）；只有显式 tls=False 才显示 http。
    assert access_url(8443, "192.168.1.5") == "https://192.168.1.5:8443/"
    assert access_url(8443, "192.168.1.5", tls=False) == "http://192.168.1.5:8443/"


def test_web_command_carries_tls_flags(tmp_path: Path) -> None:
    """v1.5.0：默认 HTTPS，argv 必须带 --tls auto 与明文跳转端口。"""
    cmd = web_command(
        server=Path("exam-server.exe"),
        package=tmp_path / "pkg.exe",
        keyfile=tmp_path / "key.txt",
        port=8443,
        answers_dir=tmp_path / "answers",
    )
    assert cmd[cmd.index("--tls") + 1] == "auto"
    assert cmd[cmd.index("--port") + 1] == "8443"
    assert cmd[cmd.index("--http-redirect-port") + 1] == str(DEFAULT_HTTP_REDIRECT_PORT) == "8080"

    off = web_command(
        server=Path("exam-server.exe"),
        package=tmp_path / "pkg.exe",
        keyfile=tmp_path / "key.txt",
        port=8443,
        answers_dir=tmp_path / "answers",
        tls="off",
    )
    assert off[off.index("--tls") + 1] == "off"


def test_redirect_port_is_disabled_when_it_equals_https_port(tmp_path: Path) -> None:
    """HTTPS 也配在 8080 时，跳转端口自动关掉（0），不拦老师。"""
    cmd = web_command(
        server=Path("exam-server.exe"),
        package=tmp_path / "pkg.exe",
        keyfile=tmp_path / "key.txt",
        port=8080,
        answers_dir=tmp_path / "answers",
        http_redirect_port=8080,
    )
    assert cmd[cmd.index("--http-redirect-port") + 1] == "0"


def test_validate_inputs_rejects_bad_tls(tmp_path: Path) -> None:
    pkg = tmp_path / "pkg.exe"
    key = tmp_path / "key.txt"
    pkg.write_bytes(b"MZ")
    key.write_text("00" * 32, encoding="utf-8")
    bad = validate_inputs(pkg, key, 8443, tls="strict")
    assert bad is not None and "TLS" in bad
