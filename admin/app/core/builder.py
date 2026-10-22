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
) -> BuildResult:
    """调用 exam-builder 生成学生端自解压包。"""
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
        "--manifest", str(manifest),
        "--banks", str(banks_dir),
        "--stub", str(stub_exe),
        "--out", str(out_exe),
    ]
    proc = subprocess.run(  # noqa: S603
        cmd, capture_output=True, text=True, timeout=timeout, env=env
    )
    ok = proc.returncode == 0
    return BuildResult(
        ok=ok,
        output=(proc.stdout + proc.stderr).strip(),
        package=out_exe if ok else None,
    )
