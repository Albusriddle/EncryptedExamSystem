"""缺考名单：把"场次名单"和"收上来的答题卡"对起来（Qt-free）。

口径：
- `missing`    名单里有、但没有对应的卡 —— 缺考
- `unsubmitted` 有卡但没交卷（自动保存的）
- `submitted`  有卡且已交卷
- `invalid`    有卡但校验失败 / 与所选包不匹配
- `unlisted`   有卡、学号不在该场名单里（或场次不在工程里）

注意：场次名单是**保存时的快照**，与当前全局名单可能不同。
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

from app.core.models import Session, StudentClass

STATUS_LABELS = {
    "submitted": "已交卷",
    "unsubmitted": "未交卷",
    "missing": "缺考",
    "invalid": "校验失败",
    "unlisted": "名单外",
}


@dataclass
class AttendanceRow:
    session_id: int
    student_id: int
    class_name: str
    status: str
    note: str = ""


@dataclass
class SessionAttendance:
    session_id: int
    roster: int = 0
    submitted: int = 0
    unsubmitted: int = 0
    missing: int = 0
    invalid: int = 0
    unlisted: int = 0
    rows: list[AttendanceRow] = field(default_factory=list)


@dataclass
class AttendanceResult:
    sessions: list[SessionAttendance]
    invalid_files: list[tuple[str, str]] = field(default_factory=list)


def _class_of(classes: list[StudentClass], sid: int) -> str:
    return "、".join(c.name for c in classes if sid in c.students)


def _best(existing: dict[str, Any], new: dict[str, Any]) -> dict[str, Any]:
    """同一 (学号, 场次) 有多份卡时，优先已交卷，其次最新。"""

    def key(x: dict[str, Any]) -> tuple[int, int]:
        return (
            1 if x.get("submitted") else 0,
            int(x.get("submitted_unix") or x.get("updated_unix") or 0),
        )

    if key(new) > key(existing):
        new = dict(new)
        new["_dupes"] = existing.get("_dupes", 1) + 1
        return new
    existing["_dupes"] = existing.get("_dupes", 1) + 1
    return existing


def build_attendance(
    sessions: list[Session],
    records: list[dict[str, Any]],
    classes: list[StudentClass] | None = None,
    include_empty_sessions: bool = False,
) -> AttendanceResult:
    """records: [{"file","ok","binding","sheet","message"}]"""
    classes = classes or []
    by_id = {s.session_id: s for s in sessions}

    present: dict[tuple[int, int], dict[str, Any]] = {}
    invalid: dict[tuple[int, int], dict[str, Any]] = {}
    invalid_files: list[tuple[str, str]] = []
    unknown_sessions: list[dict[str, Any]] = []

    for r in records:
        sheet = r.get("sheet") or {}
        if not r.get("ok"):
            invalid_files.append((str(r.get("file", "")), str(r.get("message", "校验失败"))))
            continue
        sid = int(sheet.get("student_id") or 0)
        sess = int(sheet.get("session_id") or 0)
        if r.get("binding") is False:
            invalid[(sid, sess)] = {**sheet, "_note": "与所选包不匹配"}
            continue
        if sess not in by_id:
            unknown_sessions.append(sheet)
            continue
        key = (sid, sess)
        if key in present:
            present[key] = _best(present[key], sheet)
        else:
            present[key] = {**sheet, "_dupes": 1}

    out: list[SessionAttendance] = []
    for sid in sorted(by_id):
        s = by_id[sid]
        roster = list(dict.fromkeys(int(x) for x in s.students))
        has_any = any(k[1] == sid for k in present) or any(k[1] == sid for k in invalid)
        if not has_any and not include_empty_sessions:
            continue

        item = SessionAttendance(session_id=sid, roster=len(roster))
        seen: set[int] = set()
        for stu in sorted(roster):
            seen.add(stu)
            row = AttendanceRow(sid, stu, _class_of(classes, stu), "missing")
            if (stu, sid) in present:
                sheet = present[(stu, sid)]
                submitted = bool(sheet.get("submitted"))
                row.status = "submitted" if submitted else "unsubmitted"
                if submitted:
                    item.submitted += 1
                else:
                    item.unsubmitted += 1
                dupes = int(sheet.get("_dupes", 1))
                if dupes > 1:
                    row.note = f"重复文件 {dupes} 份（取最新）"
            elif (stu, sid) in invalid:
                row.status = "invalid"
                row.note = str(invalid[(stu, sid)].get("_note", "校验失败"))
                item.invalid += 1
            else:
                item.missing += 1
            item.rows.append(row)

        for (stu, sess), _sheet in sorted(present.items()):
            if sess == sid and stu not in seen:
                item.rows.append(
                    AttendanceRow(sid, stu, _class_of(classes, stu), "unlisted", "学号不在本场名单")
                )
                item.unlisted += 1
        for (stu, sess), _sheet in sorted(invalid.items()):
            if sess == sid and stu not in seen:
                item.rows.append(
                    AttendanceRow(sid, stu, _class_of(classes, stu), "unlisted", "学号不在本场名单")
                )
                item.unlisted += 1
        out.append(item)

    for sheet in unknown_sessions:
        sid = int(sheet.get("session_id") or 0)
        stu = int(sheet.get("student_id") or 0)
        item = next((x for x in out if x.session_id == sid), None)
        if item is None:
            item = SessionAttendance(session_id=sid)
            out.append(item)
        item.rows.append(
            AttendanceRow(sid, stu, _class_of(classes, stu), "unlisted", "场次不在工程里")
        )
        item.unlisted += 1

    return AttendanceResult(sessions=out, invalid_files=invalid_files)


def csv_rows(result: AttendanceResult) -> list[list[str]]:
    rows: list[list[str]] = []
    for sess in result.sessions:
        for r in sess.rows:
            if r.status == "submitted":
                continue
            rows.append(
                [
                    str(r.session_id),
                    str(r.student_id),
                    r.class_name,
                    STATUS_LABELS[r.status],
                    r.note,
                ]
            )
    for name, msg in result.invalid_files:
        rows.append(["", "", "", "文件无法解析", f"{name}: {msg}"])
    return rows
