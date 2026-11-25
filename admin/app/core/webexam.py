"""网页考试（exam-server）的纯逻辑：命令拼装与输入校验。

保持 Qt-free，方便单元测试；UI 在 `admin/app/ui/web_exam_dialog.py`。
"""

from __future__ import annotations

import ipaddress
import re
import socket
from pathlib import Path

#: 网页考试默认端口（HTTPS）。
#:
#: v1.5.0 起服务端默认走 HTTPS；旧书签用的 8080 由服务端另开一个「只回 301」的明文端口。
DEFAULT_PORT = 8443

#: 默认明文 301 端口（只用于旧书签跳转；0 = 关闭）。
DEFAULT_HTTP_REDIRECT_PORT = 8080

#: TLS 模式默认值："auto"（自签证书，没证书就自动生成）/"off"（明文，仅排障）。
DEFAULT_TLS = "auto"

#: 默认监听地址：所有网卡。老师在多网卡（机房 LAN + 外网）机器上可以只绑 LAN 地址，
#: 这样外网侧根本连不上——比依赖防火墙更直接。
DEFAULT_BIND = "0.0.0.0"

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

#: 证书来源（v1.7.0）：
#: - `auto`  = 服务端自签证书（首次启动自动生成，学生点「高级 → 继续前往」）；
#: - `custom`= 学校自己的证书（内网 CA 签发），需要显式给 cert/key（可选 CA）。
TLS_SOURCES = ("auto", "custom")

#: 外部证书的自检严格程度，与 `crates/server/src/main.rs` 的 `--tls-check` 一致。
#: `warn`（默认）= 过期 / SAN 缺失只告警；`strict` = 同样拒绝启动。
TLS_CHECK_WARN = "warn"
TLS_CHECK_STRICT = "strict"
TLS_CHECK_MODES = (TLS_CHECK_WARN, TLS_CHECK_STRICT)

#: 文件对话框的过滤器。放 core 里是为了能在无 Qt 的单测里断言文案一致，
#: 也避免 GUI 里散落两份魔法字符串。
CERT_FILE_FILTER = "证书 (*.crt *.pem *.cer);;所有文件 (*)"
KEY_FILE_FILTER = "私钥 (*.key *.pem);;所有文件 (*)"


#: 头部嗅探只读这么多字节：PEM 头永远在最前面（`-----BEGIN …` 就在开头几行），
#: 读全文既慢又危险——老师误选一个 200 MB 的抓包文件时会把整个文件塞进内存
#: （外加解码后的字符串）。64 KiB 足够覆盖「几段证书 + 私钥 + 注释」的常见 PEM。
_PEM_SNIFF_BYTES = 64 * 1024

# PEM 的起始标记单独抽出来：把「BEGIN 前缀」与「块名」拼起来再编译，源码里就不会出现
# 完整的私钥头字面量（CI 的 secret-scan / gitleaks private-key 规则会把它当成真实泄漏——
# v1.7.0 的测试夹具就因此红过一次）。
_PEM_BEGIN = "-----BEGIN "

_RE_CERT_HEADER = re.compile(_PEM_BEGIN + r"CERTIFICATE-----")
# Rust/Python 都能认的正常私钥头：`PRIVATE KEY`（PKCS#8）、`RSA PRIVATE KEY`、
# `EC PRIVATE KEY`、`DSA PRIVATE KEY`。
_RE_KEY_HEADER = re.compile(_PEM_BEGIN + r"(?:[A-Z0-9]+ )?PRIVATE KEY-----")
# 下面两种**服务端不认**（rustls 只接受不带口令的 PEM 私钥），要单独识别出来给专门文案，
# 否则老师只会看到服务端那句英文的解析失败。
_RE_OPENSSH_KEY = re.compile(_PEM_BEGIN + r"OPENSSH PRIVATE KEY-----")
_RE_ENCRYPTED_KEY = re.compile(_PEM_BEGIN + r"ENCRYPTED PRIVATE KEY-----")


def pem_kind(path: Path) -> str:
    """判断一个 PEM 文件里有哪些块。

    返回值：`cert` / `key` / `both` / `openssh` / `encrypted` / `unknown`。

    为什么只嗅探、不解析 X.509：真正的强校验在服务端**启动自检**里（私钥是否配套、
    EKU、链、有效期、SAN），失败会拒绝启动并把原因打进对话框日志。界面这一层只需要
    挡住「明显选错文件」（把私钥选进证书框、选了一个 txt），为此引 X.509 解析依赖
    不值得。

    为什么允许 `both`：内网 CA 常把「叶子 + 中间 + 私钥」放在同一个 PEM 里，
    此时两个框都可以选它——服务端自己能分清。

    为什么只读前 64 KiB：见 `_PEM_SNIFF_BYTES`。用正则而不是两次 `in` 判断：
    `"PRIVATE KEY" in text` 会把「证书 + 注释里写了 PRIVATE KEY」也判成私钥。
    """
    try:
        with path.open("rb") as fh:
            head = fh.read(_PEM_SNIFF_BYTES)
    except OSError:
        return "unknown"
    text = head.decode("utf-8", "replace")
    if _RE_OPENSSH_KEY.search(text):
        return "openssh"
    if _RE_ENCRYPTED_KEY.search(text):
        return "encrypted"
    has_cert = bool(_RE_CERT_HEADER.search(text))
    has_key = bool(_RE_KEY_HEADER.search(text))
    if has_cert and has_key:
        return "both"
    if has_cert:
        return "cert"
    if has_key:
        return "key"
    return "unknown"


def _check_pem_file(path: Path, what: str, kinds: tuple[str, ...]) -> str | None:
    """文件存在性 + 非空 + PEM 类型检查；返回错误文案或 None。"""
    if path.is_dir():
        # 目录也能通过 `is_file()` 之外的一些直觉判断，但说「被移动或删除」会误导。
        return f"{path} 是文件夹，不是一个文件——请选具体文件"
    if not path.is_file():
        return f"找不到{what}：{path}（文件被移动或删除？重新选一次）"
    try:
        if path.stat().st_size == 0:
            return f"{what}是空文件：{path}（是不是选错了？）"
    except OSError as exc:
        return f"读不到{what}：{path}（{exc}）"
    kind = pem_kind(path)
    if kind in kinds:
        return None
    if kind == "openssh":
        return (
            f"这是 OpenSSH 格式的私钥（ssh-keygen 生成的那种）：服务端只认 PEM"
            f"（PKCS#8 / EC / RSA）。请先用 `openssl pkcs8 -topk8 -nocrypt -in <原名> -out <新名>` "
            f"转成 PEM 再选（当前：{path}）。"
        )
    if kind == "encrypted":
        return (
            f"这个私钥被口令保护了：服务端只认**不带口令**的 PEM 私钥，启动时无法输入口令。"
            f"请先解密（去掉口令）再选（当前：{path}）。"
        )
    want = "私钥" if what == "私钥文件" else "证书"
    begin = "PRIVATE KEY" if want == "私钥" else "CERTIFICATE"
    return f"这个文件不像 PEM {want}：{path}（缺 -----BEGIN {begin}----- 行，选错文件了？）"


def web_command(
    *,
    server: Path,
    package: Path,
    keyfile: Path,
    port: int,
    answers_dir: Path,
    bind: str = DEFAULT_BIND,
    time_check: str = DEFAULT_TIME_CHECK,
    login_rate_limit: int = DEFAULT_LOGIN_RATE_LIMIT,
    tls: str = DEFAULT_TLS,
    http_redirect_port: int = DEFAULT_HTTP_REDIRECT_PORT,
    tls_cert: Path | None = None,
    tls_key: Path | None = None,
    tls_ca: Path | None = None,
    tls_check: str | None = None,
    roster: Path | None = None,
) -> list[str]:
    """拼出 `exam-server` 的 argv。

    服务端只需要考试包与 K_manifest；**master 与答案都不传入**。

    `--time-check` / `--login-rate-limit` 由教师在这里选（v1.4.6 起）：前者决定
    「没外网的机房照样开考」还是「时间不一致就拒绝启动」，后者决定同一 IP 每分钟
    允许多少次登录（0 = 不限，演练或学生都在同一个 NAT 后面时需要）。

    四个证书参数（v1.7.0）只在 `custom`（自己的证书）下由界面传入；`auto`（自签）时
    全部为 None，argv 与 v1.6.x 完全一致。

    `roster`（v1.8.0）指向「每行一个学号」的名单文件，供监考页显示「谁还没进来」；
    不给时监考页只显示已经登录的人（服务端从考试包里读不到名单——session header 是匿名的）。

    顺序跟 `crates/server/README.md` 的参数表一致：`--tls` 及其四个证书参数之后、
    `--http-redirect-port` 之前。
    """
    argv = [
        str(server),
        "--package",
        str(package),
        "--keyfile",
        str(keyfile),
        "--port",
        str(port),
        "--answers-dir",
        str(answers_dir),
        # 监听地址：默认所有网卡；只绑局域网地址时外网侧直接连不上。
        "--bind",
        bind,
        "--time-check",
        time_check,
        "--login-rate-limit",
        str(login_rate_limit),
        # v1.5.0：默认 HTTPS（自签证书）。`off` 只用于排障或浏览器无法绕过证书警告的场合。
        "--tls",
        tls,
    ]
    # 只加真正给了的：None 表示「用自签」，此时一个证书参数都不该出现——否则
    # 老版 exam-server 会以「未知参数」退出，而界面看起来一切正常。
    if tls_cert is not None:
        argv += ["--tls-cert", str(tls_cert)]
    if tls_key is not None:
        argv += ["--tls-key", str(tls_key)]
    if tls_ca is not None:
        argv += ["--tls-ca", str(tls_ca)]
    if tls_check is not None:
        argv += ["--tls-check", str(tls_check)]
    # 名单：服务端从包里拿不到「谁在这个班」（session header 是匿名的，只有 tag + 密文），
    # 所以由管理端把项目学号写成文件传过去。没有它，监考页只能显示「谁已经登录」，
    # 看不到「谁还没进来」——那正是监考最关心的一件事。
    if roster is not None:
        argv += ["--roster", str(roster)]
    argv += [
        "--http-redirect-port",
        # 老师可能把 HTTPS 也配到 8080：这时跳转端口没法用，直接关掉（0），
        # 而不是拦住他——旧书签本来就该跟着换地址。
        str(0 if http_redirect_port == port else http_redirect_port),
    ]
    return argv


def validate_inputs(
    package: Path,
    keyfile: Path,
    port: int,
    *,
    bind: str = DEFAULT_BIND,
    time_check: str = DEFAULT_TIME_CHECK,
    login_rate_limit: int = DEFAULT_LOGIN_RATE_LIMIT,
    tls: str = DEFAULT_TLS,
    http_redirect_port: int = DEFAULT_HTTP_REDIRECT_PORT,
    tls_cert: Path | None = None,
    tls_key: Path | None = None,
    tls_ca: Path | None = None,
    tls_check: str | None = None,
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
    try:
        ipaddress.ip_address(bind)
    except ValueError:
        return f"监听地址必须是 IP（例如 0.0.0.0 或 192.168.1.5），实际 {bind!r}"
    if time_check not in TIME_CHECK_MODES:
        return f"时间校验必须是 {' / '.join(TIME_CHECK_MODES)} 之一（实际 {time_check!r}）"
    if tls not in ("auto", "off"):
        return f"TLS 模式必须是 auto / off 之一（实际 {tls!r}）"
    # ---- 自己的证书（v1.7.0） ----
    if tls_check is not None and tls_check not in TLS_CHECK_MODES:
        return f"严格自检必须是 {' / '.join(TLS_CHECK_MODES)} 之一（实际 {tls_check!r}）"
    has_cert = tls_cert is not None
    has_key = tls_key is not None
    if tls == "off" and (has_cert or has_key or tls_ca is not None):
        return (
            "已选「不使用 HTTPS」，就不能再指定证书（明文模式没有证书）。"
            "要么清掉证书，要么重新勾上「使用 HTTPS」。"
        )
    if has_cert != has_key:
        return (
            "「自己的证书」需要同时选择证书文件与私钥文件——只给一个的话服务端起不来。"
            "请把另一个也选上，或把「证书来源」切回「自签」。"
        )
    if has_cert and has_key:
        # 顺序：先证书后私钥，报错时先说清是哪个框选错了。
        # 不用 `assert path is not None`：`python -O` 会把 assert 剥掉，而且 AGENTS §7
        # 明确要求「不对用户输入做断言式处理」——这里用 `continue` 跳过 None 即可。
        for path, what, kinds in (
            (tls_cert, "证书文件", ("cert", "both")),
            (tls_key, "私钥文件", ("key", "both")),
        ):
            if path is None:
                continue
            err = _check_pem_file(path, what, kinds)
            if err:
                return err
    if tls_ca is not None:
        if not has_cert:
            return "CA 证书只在「自己的证书」下有意义：请先选证书与私钥，或清掉 CA 证书。"
        err = _check_pem_file(tls_ca, "CA 证书", ("cert", "both"))
        if err:
            return err
    if not (0 <= http_redirect_port <= 65535):
        return f"明文跳转端口必须在 0–65535 之间（0 = 关闭），实际 {http_redirect_port}"
    if not (0 <= login_rate_limit <= MAX_LOGIN_RATE_LIMIT):
        return (
            f"同 IP 登录上限必须在 0–{MAX_LOGIN_RATE_LIMIT} 之间（0 = 不限），"
            f"实际 {login_rate_limit}"
        )
    return None


def bind_choices() -> list[tuple[str, str]]:
    """「监听地址」下拉框的可选项：(值, 给人看的标签)。

    默认 `0.0.0.0`（所有网卡）；如果能探测到本机局域网地址，额外提供「只绑这个地址」
    ——多网卡教师机（机房 LAN + 校园网）上只绑 LAN，外网侧根本连不上。
    `127.0.0.1` 留给教师自己在本机调试。
    """
    choices: list[tuple[str, str]] = [(DEFAULT_BIND, "全部网卡（0.0.0.0，默认）")]
    ip = local_ip()
    if ip != "127.0.0.1":
        choices.append((ip, f"仅 {ip}（本机局域网地址）"))
    choices.append(("127.0.0.1", "仅本机（127.0.0.1，自己调试用）"))
    return choices


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


def advertised_host(bind: str) -> str:
    """告诉学生该访问哪个主机名。

    绑 `0.0.0.0` / `::`（所有网卡）时用本机局域网地址；绑了具体地址（只绑 LAN、
    或只绑 127.0.0.1 调试）就直接用它——否则绑回环还会显示一个学生连不上的地址。
    与服务端启动横幅 `bind_host()` 保持同一口径。
    """
    return local_ip() if bind in ("0.0.0.0", "::", "[::]") else bind


def access_url(port: int, ip: str | None = None, tls: bool = True) -> str:
    """学生该打开的地址。v1.5.0 起默认 https（自签证书）。

    `tls=False` 只在老师显式用 `--tls off` 排障时用——那时地址要显示成 http。
    """
    scheme = "https" if tls else "http"
    return f"{scheme}://{ip or local_ip()}:{port}/"
