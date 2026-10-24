"""把一张图片转成可粘进题库的 data URI 题干片段。

用法：
    python tools/embed-image.py 图.png --alt 示意图

输出形如：
    ![示意图](data:image/png;base64,iVBORw0...)

为什么要内嵌：考试包必须离线自包含（拷一个 exe 到考场就能考），
图片走外部文件会带来路径穿越和临时文件残留问题。
"""

from __future__ import annotations

import argparse
import base64
import sys
from pathlib import Path

MAX_BYTES = 1_500_000
MIME = {".png": "image/png", ".jpg": "image/jpeg", ".jpeg": "image/jpeg"}


def main() -> int:
    ap = argparse.ArgumentParser(description="把图片转成 data URI（题干用）")
    ap.add_argument("image", type=Path)
    ap.add_argument("--alt", default="", help="图片说明（读屏与解码失败时显示）")
    args = ap.parse_args()

    mime = MIME.get(args.image.suffix.lower())
    if mime is None:
        print(f"只支持 PNG/JPEG，收到：{args.image.suffix}", file=sys.stderr)
        return 2

    data = args.image.read_bytes()
    if len(data) > MAX_BYTES:
        print(
            f"图片 {len(data) / 1024:.0f} KB 超过 {MAX_BYTES // 1000} KB 上限。\\n"
            "请先缩小到 1200px 宽以内再试（照片建议存成 JPEG）。",
            file=sys.stderr,
        )
        return 3

    b64 = base64.b64encode(data).decode("ascii")
    print(f"![{args.alt}](data:{mime};base64,{b64})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
