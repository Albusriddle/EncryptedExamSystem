"""调用 exam-builder 打包。

密钥**绝不走命令行参数**：走环境变量 EXAM_MASTER，
避免出现在进程列表里被同机其他用户读到。
"""

from __future__ import annotations

import os
import subprocess
from dataclasses import dataclass
from pathlib import Path


@dataclass
class BuildResult:
    ok: bool
    output: str
    package: Path | None = None
    #: 若请求了 --emit-keyfile，这里是写出的 K_manifest 文件路径
    keyfile: Path | None = None
    #: 包内是否内嵌了密钥（决定学生能否零配置使用）
    embedded: bool = False


class BuilderError(RuntimeError):
    pass


def build_package(
    builder_exe: Path,
    manifest: Path,
    banks_dir: Path,
    stub_exe: Path,
    out_exe: Path,
    master_hex: str,
    timeout: int = 300,
    *,
    emit_keyfile: Path | None = None,
    embed_key: bool = True,
) -> BuildResult:
    """调用 exam-builder 生成学生端自解压包。

    参数与 CLI 的 `exam-builder build` 一一对应，避免"UI 能做的脚本做不了"：

    - `emit_keyfile`：额外导出 K_manifest（可选，用于自行编译内嵌版 client）；
    - `embed_key`：是否把密钥写进包（默认 True，学生零配置）。
    """
    if not builder_exe.exists():
        raise BuilderError(f"找不到 builder: {builder_exe}")
    if not stub_exe.exists():
        raise BuilderError(f"找不到学生端 stub: {stub_exe}")
    if len(master_hex) != 64:
        raise BuilderError("主密钥必须是 64 位十六进制字符串")

    env = {**os.environ, "EXAM_MASTER": master_hex}
    cmd = [
        str(builder_exe),
        "build",
        "--manifest",
        str(manifest),
        "--banks",
        str(banks_dir),
        "--stub",
        str(stub_exe),
        "--out",
        str(out_exe),
    ]
    if emit_keyfile is not None:
        cmd += ["--emit-keyfile", str(emit_keyfile)]
    if not embed_key:
        cmd += ["--no-embed-key"]

    # 必须显式指定 UTF-8：Rust 端输出中文，而 Windows 上 `text=True`
    # 默认用系统 locale（cp936/GBK）解码，遇到 UTF-8 中文会抛
    # UnicodeDecodeError 或产生乱码。errors="replace" 保证即使有异常字节
    # 也不会让整个打包流程崩在"读输出"这一步。
    proc = subprocess.run(  # noqa: S603
        cmd,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=timeout,
        env=env,
    )
    ok = proc.returncode == 0
    combined = (proc.stdout + proc.stderr).strip()
    return BuildResult(
        ok=ok,
        output=combined,
        package=out_exe if ok else None,
        keyfile=emit_keyfile if (ok and emit_keyfile is not None) else None,
        # 从输出里判断，而不是想当然：以实际行为为准
        embedded=ok and "已内嵌密钥" in combined,
    )
