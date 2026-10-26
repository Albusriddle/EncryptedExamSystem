"""审计日志哈希链。"""

from __future__ import annotations

from pathlib import Path

from app.core.audit import AuditLog, verify_file


def test_append_and_verify(tmp_path: Path) -> None:
    log = AuditLog(tmp_path)
    log.append("open_project", path="a.eesproj")
    log.append("manual_score", student=1, qid="q1", old=0, new=2)
    assert verify_file(log.path)
    lines = log.path.read_text(encoding="utf-8").splitlines()
    assert len(lines) == 2
    assert '"prev":""' in lines[0] or '"prev": ""' in lines[0]


def test_tamper_detected(tmp_path: Path) -> None:
    log = AuditLog(tmp_path)
    log.append("open_project", path="a.eesproj")
    log.append("save_project", path="a.eesproj")
    text = log.path.read_text(encoding="utf-8").replace('"a.eesproj"', '"b.eesproj"', 1)
    log.path.write_text(text, encoding="utf-8")
    assert not verify_file(log.path)
