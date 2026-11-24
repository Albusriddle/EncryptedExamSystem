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


def test_rejects_absurd_kdf_header_before_argon2() -> None:
    blob = (
        secure.MAGIC_PROJECT
        + b"\x01"
        + (65535).to_bytes(2, "big")
        + (4_000_000).to_bytes(4, "big")
        + b"\x08"
        + b"0" * secure.SALT_LEN
        + b"1" * secure.NONCE_LEN
        + b"x" * 16
    )
    with pytest.raises(ValueError, match="KDF"):
        secure.decrypt_project(blob, "pw")


# ---------------------------------------------------------------- 原子写


def test_atomic_write_replaces_and_cleans_tmp(tmp_path: Path) -> None:
    target = tmp_path / "k.eekey"
    target.write_bytes(b"OLD")
    secure.atomic_write(target, b"NEW")
    assert target.read_bytes() == b"NEW"
    assert not (tmp_path / "k.eekey.tmp").exists(), "临时文件必须被替换掉"


def test_atomic_write_keeps_old_file_when_fsync_fails(tmp_path: Path, monkeypatch) -> None:
    """fsync 失败时目标文件必须还是旧内容，且不留下 .tmp。

    为什么重要：`.eekey` 是主密钥的**唯一备份**；如果「写一半失败」把目标文件
    覆盖成半截，老师就同时失去了新备份和旧备份，而且要到恢复时才看得出来。
    """

    def boom(_fd: int) -> None:
        raise OSError(28, "No space left on device")

    target = tmp_path / "k.eekey"
    target.write_bytes(b"OLD")
    monkeypatch.setattr(secure.os, "fsync", boom)
    with pytest.raises(OSError):
        secure.backup_master_key("ab" * 32, target, "pw")
    assert target.read_bytes() == b"OLD", "失败时不能碰目标文件"
    assert not (tmp_path / "k.eekey.tmp").exists(), "失败时必须清理临时文件"
