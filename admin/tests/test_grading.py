"""自动判分：四种题型 + 归一化 + 部分分。"""

from __future__ import annotations

from app.core.grading import Policy, grade_question, grade_sheet, summarize
from app.core.grading import Policy as P
from app.core.models import Question


def _q(qtype: str, answer, score: int = 2) -> Question:
    return Question(id="q", type=qtype, stem="s", answer=answer, score=score)


def test_single_is_case_insensitive_by_default() -> None:
    q = _q("single", {"kind": "single", "answer": "B"})
    assert grade_question(q, "b", P())[:2] == (2, "correct")
    assert grade_question(q, "A", P())[:2] == (0, "wrong")
    strict = Policy(ignore_case=False)
    assert grade_question(q, "b", strict)[:2] == (0, "wrong")


def test_multiple_compares_as_set() -> None:
    q = _q("multiple", {"kind": "multi", "answer": ["A", "C"]}, score=3)
    assert grade_question(q, "CA", P())[:2] == (3, "correct")
    assert grade_question(q, "AC", P())[:2] == (3, "correct")
    assert grade_question(q, "A", P())[:2] == (0, "wrong")
    assert grade_question(q, "ABD", P())[:2] == (0, "wrong")


def test_fill_all_or_nothing_by_default() -> None:
    q = _q("fill", {"kind": "blanks", "answer": [{"no": "①", "answer": "a + b"}]}, score=4)
    fill = Policy(enabled=frozenset({"fill"}))
    assert grade_question(q, '["a+b"]', fill)[:2] == (4, "correct")
    assert grade_question(q, '["a + c"]', fill)[:2] == (0, "wrong")


def test_fill_partial_credit_when_enabled() -> None:
    q = _q(
        "fill",
        {"kind": "blanks", "answer": [{"no": "①", "answer": "1"}, {"no": "②", "answer": "2"}]},
        score=4,
    )
    partial = Policy(enabled=frozenset({"fill"}), blank_partial=True)
    assert grade_question(q, '["1", "3"]', partial)[:2] == (2, "wrong")
    assert grade_question(q, '["1", "2"]', partial)[:2] == (4, "correct")


def test_blank_answer_is_marked_blank() -> None:
    q = _q("single", {"kind": "single", "answer": "B"})
    assert grade_question(q, None, P())[:2] == (0, "blank")
    assert grade_question(q, "", P())[:2] == (0, "blank")


def test_fix_compares_by_line() -> None:
    q = _q(
        "fix",
        {"kind": "fixes", "answer": [{"line": 3, "answer": "return x * x;"}]},
        score=5,
    )
    ok = '[{"line":"3","answer":"return x * x;"}]'
    bad_line = '[{"line":"2","answer":"return x * x;"}]'
    assert grade_question(q, ok, Policy(enabled=frozenset({"fix"})))[:2] == (5, "correct")
    assert grade_question(q, bad_line, Policy(enabled=frozenset({"fix"})))[:2] == (0, "wrong")


def test_disabled_type_is_unjudged() -> None:
    q = _q("fill", {"kind": "blanks", "answer": [{"no": "①", "answer": "x"}]})
    assert grade_question(q, '["x"]', Policy(enabled=frozenset({"single"})))[1] == "unjudged"


def test_grade_sheet_and_summary() -> None:
    q1 = _q("single", {"kind": "single", "answer": "A"}, score=2)
    q2 = _q("multiple", {"kind": "multi", "answer": ["A", "C"]}, score=3)
    bank = {"q1": q1, "q2": q2}
    sheet = {
        "student_id": 2,
        "session_id": 1,
        "qids": ["q1", "q2", "q9"],
        "answers": ["A", "AC", "x"],
    }
    results = grade_sheet(bank, sheet, P())
    assert [r.status for r in results] == ["correct", "correct", "unknown"]
    s = summarize(results)
    assert s["judged"] == 2 and s["score"] == 5 and s["max_score"] == 5
    assert s["unjudged"] == 1


def test_manual_score_overrides_total() -> None:
    q = _q("single", {"kind": "single", "answer": "A"}, score=2)
    results = grade_sheet(
        {"q1": q}, {"student_id": 1, "session_id": 1, "qids": ["q1"], "answers": ["B"]}, P()
    )
    assert results[0].final_score == 0
    results[0].manual_score = 1
    assert summarize(results)["score"] == 1
