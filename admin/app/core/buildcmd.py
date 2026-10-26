"""构造 `exam-builder build` 的命令行与环境变量。

## 为什么单独一个模块

这条命令行里有两条**安全约束**，都必须有单元测试守着：

1. **主密钥绝不能出现在 argv 里**——同机任何用户都能从进程列表读到，
   所以它只能走环境变量 `EXAM_MASTER`；
2. **覆盖已有输出必须显式 `--force`**——builder 默认拒绝覆盖，
   避免"上一次的包被静默换掉"。

放在 `ui/tab_build.py` 里就没法测（那里 import PySide6）。
本模块 Qt-free，测试直接调用。
"""

from __future__ import annotations

import os
from pathlib import Path

#: 传主密钥用的环境变量名（与 Rust 端一致）。
MASTER_ENV = "EXAM_MASTER"


def build_command(
    *,
    builder_exe: Path,
    manifest: Path,
    banks_dir: Path,
    stub: Path,
    out: Path,
    keyfile: Path | None = None,
    force: bool = True,
    pin_login: bool = False,
    emit_pins: Path | None = None,
) -> list[str]:
    """拼出 `exam-builder build` 的完整 argv。

    `force=True`（默认）用于管理端：输出路径由教师在保存对话框里选定，
    覆盖确认已经在界面上做过，这里放行；CLI 用户不加 `--force` 时
    builder 会拒绝覆盖。
    """
    cmd = [
        str(builder_exe),
        "build",
        "--manifest",
        str(manifest),
        "--banks",
        str(banks_dir),
        "--stub",
        str(stub),
        "--out",
        str(out),
    ]
    if force:
        cmd.append("--force")
    if pin_login:
        cmd.append("--pin-login")
        if emit_pins is not None:
            cmd += ["--emit-pins", str(emit_pins)]
    if keyfile is not None:
        cmd += ["--emit-keyfile", str(keyfile)]
    return cmd


def build_env(master_hex: str, base: dict[str, str] | None = None) -> dict[str, str]:
    """返回带主密钥的环境变量表。**密钥只在这里出现，不进 argv。**"""
    env = dict(os.environ if base is None else base)
    env[MASTER_ENV] = master_hex
    return env
