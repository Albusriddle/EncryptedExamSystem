"""tools/ 下的小工具（不依赖 Qt）。"""

from __future__ import annotations

import base64
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# 中文 Windows 下子进程默认按 GBK 输出；必须显式要求 UTF-8，
# 否则父进程按 UTF-8 解码会抛 UnicodeDecodeError、stdout 直接变成 None。
ENV = {**os.environ, "PYTHONIOENCODING": "utf-8"}
TOOL = ROOT / "tools" / "embed-image.py"

PNG_1PX = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
)


def test_embed_image_prints_data_uri() -> None:
    work = ROOT / "target" / "test-tools"
    work.mkdir(parents=True, exist_ok=True)
    img = work / "dot.png"
    img.write_bytes(PNG_1PX)

    out = subprocess.run(
        [sys.executable, str(TOOL), str(img), "--alt", "示意图"],
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=ENV,
    )
    assert out.returncode == 0, out.stderr
    assert out.stdout.startswith("![示意图](data:image/png;base64,"), out.stdout[:60]


def test_embed_image_rejects_unknown_format() -> None:
    work = ROOT / "target" / "test-tools"
    work.mkdir(parents=True, exist_ok=True)
    bad = work / "x.gif"
    bad.write_bytes(b"GIF89a")

    out = subprocess.run(
        [sys.executable, str(TOOL), str(bad)],
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=ENV,
    )
    assert out.returncode == 2
    assert "只支持 PNG/JPEG" in out.stderr
