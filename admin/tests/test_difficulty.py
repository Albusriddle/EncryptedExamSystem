"""平均难度区间校验。"""

from __future__ import annotations

from app.core.difficulty import check_difficulty_range
from app.core.models import ExamBank, Question


def _bank(diffs: list[int | None], title: str = "数学") -> ExamBank:
    return ExamBank(
        exam_id="m",
        title=title,
        pick=2,
        questions=[
            Question(id=f"q{i}", type="single", stem="s", answer="A", difficulty=d)
            for i, d in enumerate(diffs)
        ],
    )


def test_ok_when_range_inside_reachable() -> None:
    assert check_difficulty_range(_bank([1, 2, 3, 4]), 2, 1, 4) == []


def test_missing_difficulty_is_reported() -> None:
    problems = check_difficulty_range(_bank([1, None, 0, 3]), 2, 1, 3)
    assert problems and "未标注难度" in problems[0]


def test_unreachable_range_is_reported() -> None:
    problems = check_difficulty_range(_bank([1, 1, 1, 1]), 2, 4, 5)
    assert problems and "可达平均难度" in problems[0]
    assert "1.00–1.00" in problems[0]


def test_invalid_bounds() -> None:
    assert check_difficulty_range(_bank([1, 2]), 2, 5, 3)


def test_gap_range_is_rejected_by_exact_check() -> None:
    """{1,3} 抽 1 题、区间 2–2：min/max 看着可行，实际没有任何子集。"""
    problems = check_difficulty_range(_bank([1, 3]), 1, 2, 2)
    assert problems, "区间内没有可行组合时必须报错"
    assert "没有任何" in problems[0] or "无法满足" in problems[0]


def test_exact_check_accepts_boundary() -> None:
    assert check_difficulty_range(_bank([1, 3]), 1, 1, 1) == []
    assert check_difficulty_range(_bank([1, 3]), 1, 3, 3) == []
