"""schemas/ 与 pydantic 模型的一致性检查。

为什么需要它：`schemas/*.json` 是给教师写题库时用的（编辑器补全/校验）。
模型改了却忘记重新生成，schema 就会悄悄过期，比没有更糟——
教师按 schema 写的字段被程序拒绝。

修法：`cd admin && uv run python buildtools/gen_schemas.py`
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]


def test_schemas_match_models() -> None:
    gen = pytest.importorskip("buildtools.gen_schemas")

    rendered = gen.render_all()
    assert rendered, "没有生成任何 schema"

    for name, text in rendered.items():
        path = ROOT / "schemas" / name
        assert path.is_file(), (
            f"缺少 schemas/{name}——运行 `cd admin && uv run python buildtools/gen_schemas.py`"
        )
        on_disk = json.loads(path.read_text(encoding="utf-8"))
        current = json.loads(text)
        assert on_disk == current, f"schemas/{name} 与模型不一致——重新运行 gen_schemas.py 并提交"


def test_exam_bank_schema_has_expected_shape() -> None:
    """抽查：题库 schema 必须把 exam_id/title/pick/questions 标为必填。"""
    data = json.loads((ROOT / "schemas" / "exam-bank.schema.json").read_text(encoding="utf-8"))
    assert set(data["required"]) == {"exam_id", "title", "pick", "questions"}
    assert "Question" in data["$defs"]


def test_manifest_schema_requires_salt_and_sessions() -> None:
    data = json.loads((ROOT / "schemas" / "manifest.schema.json").read_text(encoding="utf-8"))
    assert set(data["required"]) == {"salt", "sessions"}
