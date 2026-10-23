"""仓库自带示例题库必须始终合法。

`banks/demo/` 是给教师照抄的模板：文档里引用它、README 指向它。
一旦模型改动让示例失效（比如新增必填字段），教师会照着一个导入不了的文件
去写题库——所以这里把它纳入测试。
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from app.core.models import ExamBank, Manifest

DEMO = Path(__file__).resolve().parents[2] / "banks" / "demo"


def demo_banks() -> list[Path]:
    return sorted(p for p in DEMO.glob("*.json") if p.name != "manifest.example.json")


def test_demo_dir_has_examples() -> None:
    assert demo_banks(), "banks/demo 下没有示例题库"


@pytest.mark.parametrize("path", demo_banks(), ids=lambda p: p.name)
def test_demo_bank_is_valid(path: Path) -> None:
    bank = ExamBank.model_validate_json(path.read_text(encoding="utf-8"))
    assert bank.questions, "示例题库不能是空的"
    assert bank.pick <= len(bank.questions)


def test_showcase_covers_all_body_kinds() -> None:
    path = DEMO / "demo-showcase.json"
    bank = ExamBank.model_validate_json(path.read_text(encoding="utf-8"))
    kinds = {q.body.kind for q in bank.questions}
    assert kinds == {"options", "template", "code"}, f"样例应覆盖全部正文形态，实际 {kinds}"
    types = {q.qtype for q in bank.questions}
    assert {"single", "multiple", "fill", "fix"} <= types


def test_example_manifest_is_valid() -> None:
    """示例清单不能残留已删除的字段（曾经留着 time_sources/time_pubkey）。"""
    text = (DEMO / "manifest.example.json").read_text(encoding="utf-8")
    data = json.loads(text)
    assert "time_sources" not in data
    assert "time_pubkey" not in data
    Manifest.model_validate(data)
