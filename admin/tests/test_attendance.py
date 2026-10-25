"""缺考名单：状态判定/去重/名单外/空场次。"""

from __future__ import annotations

from app.core.attendance import build_attendance, csv_rows
from app.core.models import Session, SessionExam, StudentClass


def _sessions():
    def s(i, students):
        return Session(
            session_id=i,
            start="2026-12-20T09:00:00+08:00",
            end="2026-12-20T11:00:00+08:00",
            students=students,
            exams=[SessionExam(file="m.json", pick=1)],
        )

    return [s(1, [1, 2, 3]), s(2, [4]), s(3, [5])]


def _rec(sid, sess, submitted=True, ok=True, binding=None, name="a.eea"):
    return {
        "file": name,
        "ok": ok,
        "binding": binding,
        "sheet": {"student_id": sid, "session_id": sess, "submitted": submitted, "updated_unix": 1},
        "message": "" if ok else "坏文件",
    }


def test_status_counts_and_unlisted() -> None:
    recs = [
        _rec(1, 1, True),
        _rec(2, 1, False),
        _rec(4, 2, True, binding=False),
        _rec(9, 1, True),  # 名单外
        _rec(0, 0, ok=False, name="bad.eea"),
    ]
    r = build_attendance(
        _sessions(), recs, [StudentClass(name="一班", code="C1", students=[1, 2, 3])]
    )
    by = {s.session_id: s for s in r.sessions}
    s1 = by[1]
    assert (s1.roster, s1.submitted, s1.unsubmitted, s1.missing, s1.unlisted) == (3, 1, 1, 1, 1)
    assert by[2].invalid == 1
    assert 3 not in by, "没有答题卡的场次默认跳过"
    assert len(r.invalid_files) == 1


def test_include_empty_sessions() -> None:
    r = build_attendance(_sessions(), [], [], include_empty_sessions=True)
    assert [s.session_id for s in r.sessions] == [1, 2, 3]
    assert all(s.missing == s.roster for s in r.sessions)


def test_duplicate_prefers_submitted() -> None:
    recs = [_rec(1, 1, False), _rec(1, 1, True)]
    r = build_attendance(_sessions(), recs)
    row = next(x for x in r.sessions[0].rows if x.student_id == 1)
    assert row.status == "submitted"


def test_csv_rows_skip_submitted() -> None:
    recs = [_rec(1, 1, True), _rec(2, 1, False)]
    r = build_attendance(_sessions(), recs)
    rows = csv_rows(r)
    assert all(row[1] != "1" for row in rows), "已交卷不应出现在缺考名单"
    assert any(row[1] == "2" for row in rows)
