"""场次“大题模式”：模型与工程校验。"""

from __future__ import annotations

from app.core.models import Question, QuestionBody, Section, Session
from app.core.state import ProjectState


def _bank():
    from app.core.models import ExamBank

    return ExamBank(
        exam_id="math",
        title="数学",
        pick=1,
        questions=[
            Question(
                id="q1",
                qtype="single",
                stem="1+1",
                options=["A", "B"],
                body=QuestionBody(kind="text"),
                answer="A",
                difficulty=3,
                score=5,
            ),
            Question(
                id="q2",
                qtype="single",
                stem="2+2",
                options=["A", "B"],
                body=QuestionBody(kind="text"),
                answer="A",
                difficulty=3,
                score=5,
            ),
        ],
    )


def test_section_model() -> None:
    sec = Section(section_id="s1", title="第1大题", file="math.json", count=2, difficulty=3)
    assert sec.count == 2


def test_state_accepts_sections() -> None:
    st = ProjectState()
    st.add_bank(_bank())
    st.students = [2021001]
    st.sessions = [
        Session(
            session_id=1,
            start="2026-12-20T09:00:00+08:00",
            end="2026-12-20T11:00:00+08:00",
            students=[2021001],
            exams=[],
            sections=[
                Section(
                    section_id="s1",
                    title="第1大题",
                    file="math.json",
                    count=2,
                    difficulty=3,
                )
            ],
        )
    ]
    problems = st.validate()
    assert not [p for p in problems if "大题" in p], problems
    m = st.build_manifest()
    assert m.sessions[0].sections[0].title == "第1大题"


def _session_with_sections(sections):
    return Session(
        session_id=1,
        start="2026-12-20T09:00:00+08:00",
        end="2026-12-20T11:00:00+08:00",
        students=[2021001],
        exams=[],
        sections=sections,
    )


def test_state_detects_aggregate_section_overflow() -> None:
    st = ProjectState()
    st.add_bank(_bank())
    st.students = [2021001]
    st.sessions = [
        _session_with_sections(
            [
                Section(section_id="s1", title="第1大题", file="math.json", count=2, difficulty=3),
                Section(section_id="s2", title="第2大题", file="math.json", count=2, difficulty=3),
            ]
        )
    ]
    assert any("多个大题" in p for p in st.validate()), st.validate()


def test_state_detects_duplicate_section_title() -> None:
    st = ProjectState()
    st.add_bank(_bank())
    st.students = [2021001]
    st.sessions = [
        _session_with_sections(
            [
                Section(section_id="s1", title="第1大题", file="math.json", count=1, difficulty=3),
                Section(section_id="s2", title="第1大题", file="math.json", count=1, difficulty=3),
            ]
        )
    ]
    assert any("标题" in p for p in st.validate()), st.validate()
