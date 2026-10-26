"""主密钥备份与工程加密格式。"""

from __future__ import annotations

from pathlib import Path

import pytest

from app.core import secure
from app.core.state import ProjectLocked, ProjectState


def test_key_backup_roundtrip(tmp_path: Path) -> None:
    key = "ab" * 32
    p = tmp_path / "k.eekey"
    secure.backup_master_key(key, p, "correct horse")
    assert secure.restore_master_key(p, "correct horse") == key


def test_key_backup_wrong_passphrase(tmp_path: Path) -> None:
    p = tmp_path / "k.eekey"
    secure.backup_master_key("cd" * 32, p, "right")
    with pytest.raises(ValueError, match="口令错误"):
        secure.restore_master_key(p, "wrong")


def test_project_encryption_roundtrip(tmp_path: Path) -> None:
    p = tmp_path / "p.eesproj"
    st = ProjectState(students=[1, 2])
    st.save(p, "pw-123")
    assert p.read_bytes().startswith(secure.MAGIC_PROJECT)

    with pytest.raises(ProjectLocked):
        ProjectState.load(p)
    loaded = ProjectState.load(p, "pw-123")
    assert loaded.students == [1, 2]


def test_plain_project_still_loads(tmp_path: Path) -> None:
    p = tmp_path / "plain.eesproj"
    st = ProjectState(students=[3])
    st.save(p)
    assert ProjectState.load(p).students == [3]
