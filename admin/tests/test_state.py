from __future__ import annotations

import json
from pathlib import Path

from app.core.models import ExamBank, KdfParams, Question, Session, SessionExam
from app.core.state import ProjectState


def _bank(exam_id: str = "math", n: int = 10, pick: int = 3) -> ExamBank:
    return ExamBank(
        exam_id=exam_id,
        title=f"{exam_id} 科目",
        pick=pick,
        questions=[
            Question(id=f"q{i}", type="single", stem=f"题{i}", options=["A", "B"]) for i in range(n)
        ],
    )


def _session(sid: int = 1, students: list[int] | None = None, file: str = "math.json") -> Session:
    return Session(
        session_id=sid,
        start="2026-12-20T09:00:00+08:00",
        end="2026-12-20T11:00:00+08:00",
        students=students or [2021001],
        exams=[SessionExam(file=file)],
    )


def _state() -> ProjectState:
    st = ProjectState()
    st.add_bank(_bank(), "/tmp/math.json")
    st.students = [2021001]
    st.sessions = [_session()]
    return st


# ---------------------------------------------------------------- 基础


def test_salt_is_generated_16_bytes() -> None:
    st = ProjectState()
    assert len(bytes.fromhex(st.salt)) == 16


def test_add_and_remove_bank() -> None:
    st = ProjectState()
    st.add_bank(_bank())
    assert "math" in st.banks
    st.remove_bank("math")
    assert not st.banks


def test_remove_bank_also_prunes_sessions() -> None:
    """删题库后，场次里对它的引用必须一并清掉，否则会留下悬空引用。"""
    st = _state()
    st.remove_bank("math")
    assert st.sessions[0].exams == []


def test_next_session_id() -> None:
    st = ProjectState()
    assert st.next_session_id() == 1
    st.sessions = [_session(1), _session(5)]
    assert st.next_session_id() == 6


def test_upsert_session_replaces() -> None:
    st = _state()
    s = _session()
    s.students = [9999]
    st.upsert_session(s)
    assert len(st.sessions) == 1
    assert st.sessions[0].students == [9999]


# ---------------------------------------------------------------- 校验


def test_valid_state_has_no_problems() -> None:
    assert _state().validate() == []


def test_empty_state_reports_everything_missing() -> None:
    problems = ProjectState().validate()
    text = " | ".join(problems)
    assert "题库" in text
    assert "学生" in text
    assert "场次" in text


def test_missing_bank_reference_is_reported() -> None:
    st = _state()
    st.sessions = [_session(file="nope.json")]
    problems = st.validate()
    assert any("不存在的题库" in p for p in problems)


def test_student_overlap_is_reported() -> None:
    """同一学生跨重叠场次必须报——与 Rust 端 check_student_overlap 对齐。"""
    st = _state()
    st.sessions = [
        _session(1, [2021001]),
        Session(
            session_id=2,
            start="2026-12-20T10:00:00+08:00",  # 与场次 1 重叠
            end="2026-12-20T12:00:00+08:00",
            students=[2021001],  # 同一学生
            exams=[SessionExam(file="math.json")],
        ),
    ]
    problems = st.validate()
    assert any("时间重叠" in p and "同时在两场" in p for p in problems)


def test_overlap_without_shared_students_is_ok() -> None:
    """重叠但学生无交集是合法分班，不应报错。"""
    st = _state()
    st.sessions = [
        _session(1, [2021001]),
        Session(
            session_id=2,
            start="2026-12-20T10:00:00+08:00",
            end="2026-12-20T12:00:00+08:00",
            students=[2022002],
            exams=[SessionExam(file="math.json")],
        ),
    ]
    problems = st.validate()
    assert not any("时间重叠" in p for p in problems)


def test_duplicate_student_in_session_reported() -> None:
    st = _state()
    # 绕过 Session 的 validator 直接构造，模拟从 project.json 加载到脏数据
    st.sessions[0].students = [1, 1]
    problems = st.validate()
    assert any("重复" in p for p in problems)


# ---------------------------------------------------------------- 存取


def test_save_never_writes_master(tmp_path: Path) -> None:
    """project.json 绝不能含 master。"""
    st = _state()
    p = tmp_path / "proj.json"
    st.save(p)
    text = p.read_text(encoding="utf-8")
    data = json.loads(text)
    assert "master" not in text.lower()
    assert "master" not in data


def test_save_and_load_roundtrip(tmp_path: Path) -> None:
    bank_file = tmp_path / "math.json"
    bank_file.write_text(_bank().model_dump_json(by_alias=True), encoding="utf-8")

    st = ProjectState()
    st.add_bank(_bank(), str(bank_file))
    st.students = [2021001, 2021002]
    st.sessions = [_session(1, [2021001])]
    proj = tmp_path / "proj.json"
    st.save(proj)

    loaded = ProjectState.load(proj)
    assert loaded.salt == st.salt
    assert loaded.students == st.students
    assert len(loaded.sessions) == 1
    assert "math" in loaded.banks  # 题库被重新读取


def test_load_tolerates_missing_bank_file(tmp_path: Path) -> None:
    """题库源文件被删了也要能打开工程，只是题库为空、由 UI 提示重导入。"""
    st = ProjectState()
    st.add_bank(_bank(), str(tmp_path / "gone.json"))
    proj = tmp_path / "proj.json"
    st.save(proj)

    loaded = ProjectState.load(proj)
    assert loaded.banks == {}
    assert "math" in loaded.bank_paths  # 路径记录保留


def test_kdf_default_is_strong() -> None:
    k = KdfParams()
    assert k.m_cost >= 65536
    assert k.t_cost >= 3
