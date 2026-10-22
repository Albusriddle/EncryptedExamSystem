from __future__ import annotations

import pytest

from app.core.models import ExamBank, Manifest, Question, Session, SessionExam


def _q(i: int) -> Question:
    return Question(id=f"q{i}", type="single", stem="s", options=["A", "B"], answer="A")


def _bank(n: int = 5, pick: int = 2) -> ExamBank:
    return ExamBank(exam_id="m", title="t", pick=pick, questions=[_q(i) for i in range(n)])


def _session(**kw) -> Session:
    base = dict(
        session_id=1,
        start="2026-12-20T09:00:00+08:00",
        end="2026-12-20T11:00:00+08:00",
        students=[2021001],
        exams=[SessionExam(file="m.json")],
    )
    base.update(kw)
    return Session(**base)


def test_bank_ok() -> None:
    assert _bank().pick == 2


def test_bank_pick_exceeds_pool() -> None:
    with pytest.raises(ValueError, match="超过题量"):
        _bank(n=3, pick=10)


def test_bank_duplicate_ids() -> None:
    q = _q(1)
    with pytest.raises(ValueError, match="重复题目"):
        ExamBank(exam_id="m", title="t", pick=1, questions=[q, q])


def test_student_above_i32_max_is_allowed() -> None:
    """真实学号常为 12 位（如 202312345678），必须允许。"""
    s = _session(students=[202312345678])
    assert s.students == [202312345678]


def test_student_zero_rejected() -> None:
    with pytest.raises(ValueError, match="正整数"):
        _session(students=[0])


def test_student_negative_rejected() -> None:
    with pytest.raises(ValueError, match="正整数"):
        _session(students=[-1])


def test_student_duplicate() -> None:
    with pytest.raises(ValueError, match="重复"):
        _session(students=[1, 1])


def test_session_time_order() -> None:
    with pytest.raises(ValueError, match="早于结束时间"):
        _session(start="2026-12-20T12:00:00+08:00", end="2026-12-20T09:00:00+08:00")


def test_session_no_exams() -> None:
    with pytest.raises(ValueError, match="没有任何考试科目"):
        _session(exams=[])


def test_manifest_ok() -> None:
    m = Manifest(salt="00" * 16, sessions=[_session()])
    assert m.version == 2
    assert m.kdf.m_cost >= 65536, "默认 KDF 参数不应被调低（实测 19MiB 只需 3.8 分钟即可枚举）"
    assert m.kdf.t_cost >= 3


def test_manifest_rejects_weak_kdf() -> None:
    with pytest.raises(ValueError, match="m_cost 过低"):
        Manifest(salt="00" * 16, sessions=[_session()], kdf={"m_cost": 8, "t_cost": 3, "p_cost": 1})


def test_manifest_rejects_low_t_cost() -> None:
    with pytest.raises(ValueError, match="t_cost 过低"):
        Manifest(
            salt="00" * 16,
            sessions=[_session()],
            kdf={"m_cost": 65536, "t_cost": 1, "p_cost": 1},
        )


def test_manifest_bad_salt() -> None:
    with pytest.raises(ValueError, match="十六进制"):
        Manifest(salt="zz" * 16, sessions=[_session()])


def test_manifest_short_salt() -> None:
    with pytest.raises(ValueError, match="至少 16 字节"):
        Manifest(salt="0011", sessions=[_session()])


def test_manifest_duplicate_session_ids() -> None:
    with pytest.raises(ValueError, match="场次编号重复"):
        Manifest(salt="00" * 16, sessions=[_session(), _session()])
