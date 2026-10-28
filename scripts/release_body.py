#!/usr/bin/env python3
"""只输出当前 VERSION 对应的小节，避免 GitHub Release 带全部历史。"""
from __future__ import annotations

import re
import sys
from pathlib import Path


def main() -> None:
    sys.stdout.reconfigure(encoding="utf-8")
    version = Path("VERSION").read_text(encoding="utf-8").strip()
    text = Path("docs/release-notes.md").read_text(encoding="utf-8")
    pattern = re.compile(rf"^## 更新摘要（v{re.escape(version)}）\s*$", re.MULTILINE)
    match = pattern.search(text)
    if match is None:
        raise SystemExit(f"release-notes.md 中没有 v{version} 的更新摘要")
    next_match = re.search(r"^## 更新摘要（v", text[match.end() :], re.MULTILINE)
    end = match.end() + next_match.start() if next_match else len(text)
    print(text[match.start() : end].strip())


if __name__ == "__main__":
    main()
