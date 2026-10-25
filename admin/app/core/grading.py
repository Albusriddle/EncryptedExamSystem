"""答题卡自动判分（Qt-free）。

数据来源：
- 题库：`Question.answer`（single/multi/blanks/fixes）——只在管理端存在；
- 答题卡：`AnswerSheet.answers`（与 qids 同序）。

判分口径全部集中在 `Policy` 里，并会被写进导出文件的表头，保证可追溯。
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any

from app.core.models import Question

#: 默认只判选择题（填空/改错常需要人工复核）
DEFAULT_TYPES = frozenset({"single", "multiple"})

#: 所有支持自动判分的题型
SUPPORTED_TYPES = ("single", "multiple", "fill", "fix")

TYPE_LABELS = {
    "single": "单选题",
    "multiple": "多选题",
    "fill": "填空题",
    "fix": "改错题",
}


@dataclass(frozen=True)
class Policy:
    """判分策略。"""

    enabled: frozenset[str] = DEFAULT_TYPES
    ignore_case: bool = True
    ignore_space: bool = True
    blank_partial: bool = False  # 填空题按空给分（默认整题全对）

    def describe(self) -> str:
        types = "、".join(TYPE_LABELS.get(t, t) for t in SUPPORTED_TYPES if t in self.enabled)
        flags = [
            "忽略大小写" if self.ignore_case else "区分大小写",
            "忽略空白" if self.ignore_space else "保留空白",
            "填空按空给分" if self.blank_partial else "填空整题给分",
        ]
        return f"题型：{types or '无'}；" + "；".join(flags)


@dataclass
class ItemResult:
    student_id: int
    session_id: int
    seq: int
    qid: str
    qtype: str
    student: str
    expected: str
    score: int
    max_score: int
    status: str  # correct / wrong / blank / unjudged / unknown
    #: 人工改分后的分数；None 表示未改
    manual_score: int | None = None

    @property
    def final_score(self) -> int:
        return self.manual_score if self.manual_score is not None else self.score

    @property
    def adjusted(self) -> bool:
        return self.manual_score is not None and self.manual_score != self.score


def _norm(text: str, policy: Policy) -> str:
    t = (text or "").strip()
    if policy.ignore_space:
        t = "".join(t.split())
    if policy.ignore_case:
        t = t.casefold()
    return t


def _answer_dict(q: Question) -> dict[str, Any]:
    return q.answer if isinstance(q.answer, dict) else {}


def _choice_expected(q: Question) -> str:
    ans = _answer_dict(q)
    if ans.get("kind") == "multi":
        return "".join(sorted(str(x) for x in (ans.get("answer") or [])))
    value = ans.get("answer")
    if isinstance(value, list):
        return "".join(sorted(str(x) for x in value))
    return "" if value is None else str(value)


def _parse_json_list(text: str) -> list[Any] | None:
    try:
        value = json.loads(text)
    except (TypeError, ValueError):
        return None
    return value if isinstance(value, list) else None


def _blanks_expected(q: Question) -> list[str]:
    ans = _answer_dict(q)
    items = ans.get("answer") or []
    out: list[str] = []
    for item in items:
        if isinstance(item, dict):
            out.append(str(item.get("answer", "")))
        else:
            out.append(str(item))
    return out


def _fixes_expected(q: Question) -> list[tuple[str, str]]:
    ans = _answer_dict(q)
    out: list[tuple[str, str]] = []
    for item in ans.get("answer") or []:
        if isinstance(item, dict):
            out.append((str(item.get("line", "")), str(item.get("answer", ""))))
    return out


def _student_parts(raw: str | None) -> list[str]:
    if not raw:
        return []
    parsed = _parse_json_list(raw)
    if parsed is None:
        return [raw]
    return [str(x) for x in parsed]


def grade_question(q: Question, raw: str | None, policy: Policy) -> tuple[int, str, str, str]:
    """返回 (得分, 状态, 学生答案展示, 标准答案展示)。"""
    qtype = q.qtype
    if qtype not in policy.enabled:
        return 0, "unjudged", "" if raw is None else raw, ""

    blank = raw is None or (isinstance(raw, str) and raw.strip() == "")

    if qtype in ("single", "multiple"):
        expected = _choice_expected(q)
        student = "" if raw is None else str(raw)
        if blank:
            return 0, "blank", student, expected
        if qtype == "multiple":
            ok = set(_norm(student, policy)) == set(_norm(expected, policy))
        else:
            ok = _norm(student, policy) == _norm(expected, policy)
        return (q.score if ok else 0), ("correct" if ok else "wrong"), student, expected

    if qtype == "fill":
        expected_list = _blanks_expected(q)
        got = _student_parts(raw)
        if not got:
            return 0, "blank", "" if raw is None else raw, "；".join(expected_list)
        pairs = list(zip(got, expected_list, strict=False))
        matched = sum(1 for a, b in pairs if _norm(a, policy) == _norm(b, policy))
        total = max(len(expected_list), len(got))
        if total == 0:
            return 0, "unknown", "" if raw is None else raw, ""
        if matched == total:
            return q.score, "correct", "；".join(got), "；".join(expected_list)
        if policy.blank_partial:
            score = int(round(q.score * matched / total))
        else:
            score = 0
        return score, "wrong", "；".join(got), "；".join(expected_list)

    if qtype == "fix":
        expected_list = _fixes_expected(q)
        got_raw = _parse_json_list(raw or "") or []
        got = [
            (str(x.get("line", "")), str(x.get("answer", "")))
            for x in got_raw
            if isinstance(x, dict)
        ]
        exp_display = "；".join(f"第{line}行 → {ans}" for line, ans in expected_list)
        if not got:
            return 0, "blank", "" if raw is None else raw, exp_display
        got_map = {line: ans for line, ans in got}
        matched = sum(
            1
            for line, ans in expected_list
            if line in got_map and _norm(got_map[line], policy) == _norm(ans, policy)
        )
        total = max(len(expected_list), len(got))
        if matched == total:
            return q.score, "correct", exp_display, exp_display
        if policy.blank_partial:
            return int(round(q.score * matched / total)), "wrong", exp_display, exp_display
        return 0, "wrong", exp_display, exp_display

    return 0, "unknown", "" if raw is None else raw, ""


def grade_sheet(
    bank_by_qid: dict[str, Question],
    sheet: dict[str, Any],
    policy: Policy,
) -> list[ItemResult]:
    """判一张答题卡（sheet 为 verify-answer-sheet 的 JSON）。"""
    student_id = int(sheet.get("student_id") or 0)
    session_id = int(sheet.get("session_id") or 0)
    qids = sheet.get("qids") or []
    answers = sheet.get("answers") or []

    results: list[ItemResult] = []
    for idx, qid in enumerate(qids):
        raw = answers[idx] if idx < len(answers) else None
        q = bank_by_qid.get(str(qid))
        if q is None:
            # 题库里找不到这道题：可能是换错了工程，明确标成 unknown
            results.append(
                ItemResult(
                    student_id=student_id,
                    session_id=session_id,
                    seq=idx + 1,
                    qid=str(qid),
                    qtype="",
                    student="" if raw is None else str(raw),
                    expected="",
                    score=0,
                    max_score=0,
                    status="unknown",
                )
            )
            continue
        score, status, student_disp, expected = grade_question(q, raw, policy)
        results.append(
            ItemResult(
                student_id=student_id,
                session_id=session_id,
                seq=idx + 1,
                qid=str(qid),
                qtype=q.qtype,
                student=student_disp,
                expected=expected,
                score=score,
                max_score=q.score,
                status=status,
            )
        )
    return results


def summarize(results: list[ItemResult]) -> dict[str, int]:
    judged = [r for r in results if r.status not in ("unjudged", "unknown")]
    unjudged = [r for r in results if r.status in ("unjudged", "unknown")]
    return {
        "judged": len(judged),
        "unjudged": len(unjudged),
        "score": sum(r.final_score for r in judged),
        "max_score": sum(r.max_score for r in judged),
    }
