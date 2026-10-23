"""版本号与水印（管理端）。

## 单一来源

版本号来自仓库根的 `VERSION` 文件，Rust 端通过 `include_str!` 编译期注入，
这里在运行时读取。两处必然一致，不会出现"程序里写 0.1.0、发布页写 0.1.4"。

## 冻结后的读取

PyInstaller 打包时 `VERSION` 被 `--add-data` 放进 `_internal`，
运行时经 `sys._MEIPASS` 读取；源码运行时回落到仓库根。
"""

from __future__ import annotations

import sys
from functools import lru_cache
from pathlib import Path

FALLBACK = "0.0.0"


def _candidates() -> list[Path]:
    out: list[Path] = []
    meipass = getattr(sys, "_MEIPASS", None)
    if meipass:
        out.append(Path(meipass) / "VERSION")
    # 源码运行：admin/app/version.py → 仓库根
    out.append(Path(__file__).resolve().parents[2] / "VERSION")
    # 打包后若 VERSION 被放在程序目录旁
    out.append(Path(sys.executable).parent / "VERSION")
    return out


@lru_cache(maxsize=1)
def app_version() -> str:
    """返回版本号；读不到时返回 FALLBACK（不抛异常，避免启动失败）。"""
    for p in _candidates():
        try:
            text = p.read_text(encoding="utf-8").strip()
        except OSError:
            continue
        if text:
            return text.splitlines()[0].strip()
    return FALLBACK


def watermark() -> str:
    """产品水印：`EES vX.Y.Z Powered by AlbusRiddle`。

    品牌缩写与作者取自 `branding.json`（与欢迎弹窗同一来源），
    避免两处各写一份、改了一处忘另一处。
    """
    from app.branding import branding

    b = branding()
    author = b.credits[0].name if b.credits else "AlbusRiddle"
    return f"{b.abbr} v{app_version()} Powered by {author}"
