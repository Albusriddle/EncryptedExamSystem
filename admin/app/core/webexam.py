"""网页考试（exam-server）的纯逻辑：命令拼装与输入校验。

保持 Qt-free，方便单元测试；UI 在 `admin/app/ui/web_exam_dialog.py`。
"""

from __future__ import annotations

import socket
from pathlib import Path

#: 网页考试默认端口。
DEFAULT_PORT = 8080


def web_command(
    *,
    server: Path,
    package: Path,
    keyfile: Path,
    port: int,
    answers_dir: Path,
) -> list[str]:
    """拼出 `exam-server` 的 argv。

    服务端只需要考试包与 K_manifest；**master 与答案都不传入**。
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
    ]


def validate_inputs(package: Path, keyfile: Path, port: int) -> str | None:
    """返回错误文案；None 表示可以启动。"""
    if not package.is_file():
        return "请先选择一个已构建的考试包（学生端 exe）"
    if not keyfile.is_file():
        return "缺少 K_manifest：请在「打包」页设置「导出 K_manifest」并重新打包"
    if not (1024 <= port <= 65535):
        return "端口必须在 1024–65535 之间"
    return None


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
