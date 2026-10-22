"""品牌信息（仓库地址、许可证、致谢）。

单一来源是仓库根的 `branding.json`；Rust 端用 include_str! 编译期注入，
这里在运行时读取。两端读同一份文件，文案不会漂移。
"""

from __future__ import annotations

import json
import sys
from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path


@dataclass(frozen=True)
class Credit:
    role: str
    name: str
    url: str


@dataclass(frozen=True)
class Branding:
    product: str
    abbr: str
    repo_url: str
    license: str
    license_name: str
    tagline: str
    credits_title: str
    credits: tuple[Credit, ...]
    star_hint: str

    def display_name(self) -> str:
        return f"{self.product}（{self.abbr}）"


def _candidates() -> list[Path]:
    out: list[Path] = []
    meipass = getattr(sys, "_MEIPASS", None)
    if meipass:
        out.append(Path(meipass) / "branding.json")
    # admin/app/branding.py → 仓库根
    out.append(Path(__file__).resolve().parents[2] / "branding.json")
    out.append(Path(sys.executable).parent / "branding.json")
    return out


@lru_cache(maxsize=1)
def branding() -> Branding:
    """读取品牌信息。读不到时返回内置兜底值（不抛异常，避免启动失败）。"""
    for p in _candidates():
        try:
            raw = json.loads(p.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            continue
        return Branding(
            product=raw["product"],
            abbr=raw["abbr"],
            repo_url=raw["repo_url"],
            license=raw["license"],
            license_name=raw["license_name"],
            tagline=raw["tagline"],
            credits_title=raw.get("credits_title", "致谢"),
            credits=tuple(Credit(c["role"], c["name"], c["url"]) for c in raw["credits"]),
            star_hint=raw["star_hint"],
        )
    return Branding(
        product="保密考试系统",
        abbr="EES",
        repo_url="https://github.com/Albusriddle/EncryptedExamSystem",
        license="Apache-2.0",
        license_name="Apache License 2.0",
        tagline="加密题库随包分发，学生凭学号与网络时间门禁解锁个人试卷。",
        credits_title="致谢",
        credits=(),
        star_hint="欢迎在 GitHub 点一个 Star。",
    )
