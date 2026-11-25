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


# ---------------------------------------------------------------------------
# v1.7.0：自己的证书（内网 CA）——argv 拼装与前置校验
# ---------------------------------------------------------------------------


# 测试用的假证书/私钥头一律拼接出来：gitleaks 的 private-key 规则会匹配字面量
# 「BEGIN …PRIVATE KEY」这种字面量，这些只是用来验证「格式嗅探」的假头，拼接后既不影响
# 可读性，也不会被当成真实泄漏（CI 的 secret-scan 曾经因此红过一次）。
def _pem(kind: str, body: str = "example") -> str:
    return f"-----BEGIN {kind}-----\n{body}\n-----END {kind}-----\n"


_CERT_PEM = _pem("CERTIFICATE")
_RSA_KEY_PEM = _pem("RSA PRIVATE KEY")
_EC_KEY_PEM = _pem("EC PRIVATE KEY")
_PKCS8_KEY_PEM = _pem("PRIVATE KEY")
_COMBINED_PEM = _CERT_PEM + _PKCS8_KEY_PEM


def _pkg_key(tmp_path: Path) -> tuple[Path, Path]:
    pkg = tmp_path / "pkg.exe"
    pkg.write_bytes(b"MZ")
    key = tmp_path / "key.txt"
    key.write_text("00" * 32, encoding="utf-8")
    return pkg, key


def _write(tmp_path: Path, name: str, text: str) -> Path:
    path = tmp_path / name
    path.write_text(text, encoding="utf-8")
    return path


def test_web_command_has_no_cert_flags_by_default(tmp_path: Path) -> None:
    """自签（默认）时 argv 里一个证书参数都不该出现——否则老版服务端直接报未知参数。"""
    cmd = web_command(
        server=Path("exam-server.exe"),
        package=tmp_path / "pkg.exe",
        keyfile=tmp_path / "key.txt",
        port=8443,
        answers_dir=tmp_path / "answers",
    )
    for flag in ("--tls-cert", "--tls-key", "--tls-ca", "--tls-check"):
        assert flag not in cmd, f"{flag} 不该出现在自签模式的 argv 里"


def test_web_command_carries_custom_cert_flags(tmp_path: Path) -> None:
    """自己的证书：四个参数都要进 argv，且顺序在 --tls 之后、--http-redirect-port 之前。"""
    cert = tmp_path / "证书 文件.crt"
    key = tmp_path / "私 钥.key"
    ca = tmp_path / "内网 CA.pem"
    cmd = web_command(
        server=Path("exam-server.exe"),
        package=tmp_path / "pkg.exe",
        keyfile=tmp_path / "key.txt",
        port=8443,
        answers_dir=tmp_path / "answers",
        tls_cert=cert,
        tls_key=key,
        tls_ca=ca,
        tls_check="strict",
    )
    assert cmd[cmd.index("--tls-cert") + 1] == str(cert)
    assert cmd[cmd.index("--tls-key") + 1] == str(key)
    assert cmd[cmd.index("--tls-ca") + 1] == str(ca)
    assert cmd[cmd.index("--tls-check") + 1] == "strict"
    # 顺序：--tls → 证书四参数 → --http-redirect-port（与 README 参数表一致）
    assert cmd.index("--tls") < cmd.index("--tls-cert") < cmd.index("--http-redirect-port")


def test_web_command_carries_partial_cert_flags(tmp_path: Path) -> None:
    """只给证书 + 私钥（没有 CA、没开严格）时，只出现这两个参数。"""
    cert = tmp_path / "a.crt"
    key = tmp_path / "a.key"
    cmd = web_command(
        server=Path("exam-server.exe"),
        package=tmp_path / "pkg.exe",
        keyfile=tmp_path / "key.txt",
        port=8443,
        answers_dir=tmp_path / "answers",
        tls_cert=cert,
        tls_key=key,
    )
    assert "--tls-cert" in cmd and "--tls-key" in cmd
    assert "--tls-ca" not in cmd and "--tls-check" not in cmd


def test_pem_kind_recognises_all_key_flavours(tmp_path: Path) -> None:
    """RSA / EC / PKCS#8 三种私钥头都要认，否则老师换了密钥格式就选不进来。"""
    from app.core.webexam import pem_kind

    assert pem_kind(_write(tmp_path, "c.pem", _CERT_PEM)) == "cert"
    assert pem_kind(_write(tmp_path, "rsa.key", _RSA_KEY_PEM)) == "key"
    assert pem_kind(_write(tmp_path, "ec.key", _EC_KEY_PEM)) == "key"
    assert pem_kind(_write(tmp_path, "pk8.key", _PKCS8_KEY_PEM)) == "key"
    assert pem_kind(_write(tmp_path, "all.pem", _CERT_PEM + _PKCS8_KEY_PEM)) == "both"
    assert pem_kind(_write(tmp_path, "junk.txt", "not a pem")) == "unknown"
    assert pem_kind(tmp_path / "missing.pem") == "unknown"


def test_pem_kind_ignores_the_words_private_key_outside_a_header(tmp_path: Path) -> None:
    """纯证书里出现「PRIVATE KEY」字样（注释、CN 名字）不能判成私钥。

    旧实现是 `"-----BEGIN" in text and "PRIVATE KEY" in text`，于是下面这种文件会被
    判成 `both`，可以塞进「私钥文件」框并通过界面校验；真正拦下它的只有服务端的
    「私钥文件里没有可用的私钥」。现在按完整的头正则判定。
    """
    from app.core.webexam import pem_kind

    tricky = _CERT_PEM + "# 备注：这是一份 PRIVATE KEY 的说明，不是私钥\n"
    assert pem_kind(_write(tmp_path, "tricky.pem", tricky)) == "cert"


def test_pem_kind_flags_openssh_and_encrypted_keys(tmp_path: Path) -> None:
    """服务端只认不带口令的 PEM；OpenSSH / 加密私钥要单独识别，好给专门的文案。"""
    from app.core.webexam import pem_kind

    openssh = _pem("OPENSSH PRIVATE KEY")
    encrypted = _pem("ENCRYPTED PRIVATE KEY")
    assert pem_kind(_write(tmp_path, "id_rsa", openssh)) == "openssh"
    assert pem_kind(_write(tmp_path, "enc.key", encrypted)) == "encrypted"


def test_pem_kind_only_sniffs_the_file_head(tmp_path: Path) -> None:
    """只在开头 64 KiB 里找头：私钥头藏在很后面的大文件不该被当成私钥。

    这条同时给「误选 200 MB 抓包文件」兜底——实现只读头部，不会把整个文件读进内存。
    """
    from app.core.webexam import pem_kind

    big = _CERT_PEM + ("#" + "x" * 100 + "\n") * 800 + _PKCS8_KEY_PEM
    path = _write(tmp_path, "big.pem", big)
    assert path.stat().st_size > 64 * 1024, "用例前提：文件要比嗅探窗口大"
    assert pem_kind(path) == "cert"


def test_validate_inputs_rejects_openssh_and_encrypted_keys(tmp_path: Path) -> None:
    """这两类私钥服务端 100% 不认，界面要当场拦下并说清怎么转。"""
    pkg, kf = _pkg_key(tmp_path)
    cert = _write(tmp_path, "srv.crt", _CERT_PEM)

    ssh_err = validate_inputs(
        pkg,
        kf,
        8443,
        tls_cert=cert,
        tls_key=_write(tmp_path, "id_rsa", _pem("OPENSSH PRIVATE KEY")),
    )
    assert ssh_err is not None and "OpenSSH" in ssh_err and "openssl" in ssh_err, ssh_err

    enc_err = validate_inputs(
        pkg,
        kf,
        8443,
        tls_cert=cert,
        tls_key=_write(tmp_path, "enc.key", _pem("ENCRYPTED PRIVATE KEY")),
    )
    assert enc_err is not None and "口令" in enc_err, enc_err


def test_validate_inputs_points_out_a_directory(tmp_path: Path) -> None:
    """把文件夹选进证书框时要说「这是文件夹」，不能说「文件被移动或删除」。"""
    pkg, kf = _pkg_key(tmp_path)
    folder = tmp_path / "某个文件夹"
    folder.mkdir()
    key = _write(tmp_path, "srv.key", _PKCS8_KEY_PEM)

    err = validate_inputs(pkg, kf, 8443, tls_cert=folder, tls_key=key)
    assert err is not None and "文件夹" in err, err


def test_validate_inputs_accepts_wellformed_custom_cert(tmp_path: Path) -> None:
    pkg, kf = _pkg_key(tmp_path)
    cert = _write(tmp_path, "srv.crt", _CERT_PEM)
    key = _write(tmp_path, "srv.key", _PKCS8_KEY_PEM)
    ca = _write(tmp_path, "ca.crt", _CERT_PEM)
    assert validate_inputs(pkg, kf, 8443, tls_cert=cert, tls_key=key) is None
    assert (
        validate_inputs(pkg, kf, 8443, tls_cert=cert, tls_key=key, tls_ca=ca, tls_check="strict")
        is None
    )


def test_validate_inputs_requires_cert_and_key_together(tmp_path: Path) -> None:
    pkg, kf = _pkg_key(tmp_path)
    cert = _write(tmp_path, "srv.crt", _CERT_PEM)
    key = _write(tmp_path, "srv.key", _PKCS8_KEY_PEM)

    only_cert = validate_inputs(pkg, kf, 8443, tls_cert=cert)
    assert only_cert is not None and "同时" in only_cert, only_cert

    only_key = validate_inputs(pkg, kf, 8443, tls_key=key)
    assert only_key is not None and "同时" in only_key, only_key


def test_validate_inputs_rejects_cert_in_plaintext_mode(tmp_path: Path) -> None:
    """明文模式下指定证书是自相矛盾的配置，必须拦住而不是忽略。"""
    pkg, kf = _pkg_key(tmp_path)
    cert = _write(tmp_path, "srv.crt", _CERT_PEM)
    key = _write(tmp_path, "srv.key", _PKCS8_KEY_PEM)
    msg = validate_inputs(pkg, kf, 8443, tls="off", tls_cert=cert, tls_key=key)
    assert msg is not None and "明文" in msg, msg


def test_validate_inputs_rejects_missing_and_empty_files(tmp_path: Path) -> None:
    pkg, kf = _pkg_key(tmp_path)
    key = _write(tmp_path, "srv.key", _PKCS8_KEY_PEM)
    msg = validate_inputs(pkg, kf, 8443, tls_cert=tmp_path / "nope.crt", tls_key=key)
    assert msg is not None and "找不到" in msg, msg

    empty = tmp_path / "empty.crt"
    empty.write_text("", encoding="utf-8")
    msg = validate_inputs(pkg, kf, 8443, tls_cert=empty, tls_key=key)
    assert msg is not None and "空文件" in msg, msg


def test_validate_inputs_rejects_non_pem(tmp_path: Path) -> None:
    """选成 txt / 选错框（私钥放进证书框）都要给出可操作的提示。"""
    pkg, kf = _pkg_key(tmp_path)
    junk = _write(tmp_path, "junk.txt", "just text")
    key = _write(tmp_path, "srv.key", _PKCS8_KEY_PEM)
    msg = validate_inputs(pkg, kf, 8443, tls_cert=junk, tls_key=key)
    assert msg is not None and "不像 PEM" in msg, msg

    cert = _write(tmp_path, "srv.crt", _CERT_PEM)
    swapped = validate_inputs(pkg, kf, 8443, tls_cert=cert, tls_key=cert)
    assert swapped is not None and "私钥" in swapped, swapped


def test_validate_inputs_rejects_ca_without_cert(tmp_path: Path) -> None:
    pkg, kf = _pkg_key(tmp_path)
    ca = _write(tmp_path, "ca.crt", _CERT_PEM)
    msg = validate_inputs(pkg, kf, 8443, tls_ca=ca)
    assert msg is not None and "CA" in msg, msg


def test_validate_inputs_rejects_bad_tls_check(tmp_path: Path) -> None:
    pkg, kf = _pkg_key(tmp_path)
    cert = _write(tmp_path, "srv.crt", _CERT_PEM)
    key = _write(tmp_path, "srv.key", _PKCS8_KEY_PEM)
    msg = validate_inputs(pkg, kf, 8443, tls_cert=cert, tls_key=key, tls_check="paranoid")
    assert msg is not None and "严格自检" in msg, msg


def test_web_command_carries_roster_only_when_given(tmp_path: Path) -> None:
    """名单文件只在给了的时候才进 argv。

    服务端拿不到「谁在这个班」（session header 匿名），所以监考页的「谁还没进来」
    完全依赖这个文件；反过来，没给时不能凭空塞一个路径进去。
    """
    base = dict(
        server=Path("exam-server.exe"),
        package=tmp_path / "pkg.exe",
        keyfile=tmp_path / "key.txt",
        port=8443,
        answers_dir=tmp_path / "answers",
    )
    assert "--roster" not in web_command(**base)
    roster = tmp_path / "roster.txt"
    cmd = web_command(**base, roster=roster)
    assert cmd[cmd.index("--roster") + 1] == str(roster)
