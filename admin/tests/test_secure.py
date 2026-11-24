"""主密钥备份与工程加密格式。"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

from app.core import secure
from app.core.state import ProjectLocked, ProjectState


def _leftover_tmps(tmp_path: Path) -> list[Path]:
    """任何残留的临时文件都算错（临时名现在是唯一的，不能只查固定名）。"""
    return list(tmp_path.glob("*.tmp"))


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
    assert not _leftover_tmps(tmp_path), "临时文件必须被替换掉"


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
    assert not _leftover_tmps(tmp_path), "失败时必须清理临时文件"


def test_atomic_write_uses_unique_tmp_names(tmp_path: Path, monkeypatch) -> None:
    """同一目标的两次写必须各写各的临时文件。

    固定 `<name>.tmp` 时两个写者会互相截断：A 写完 tmp、B 又把它覆盖，
    A 的 replace 就把 B 的半截内容顶上目标位置——对 `.eekey` 这种唯一备份是灾难。
    """
    seen: list[str] = []
    real = secure.os.replace

    def spy(src, dst):
        seen.append(Path(src).name)
        return real(src, dst)

    monkeypatch.setattr(secure.os, "replace", spy)
    target = tmp_path / "k.eekey"
    secure.atomic_write(target, b"ONE")
    secure.atomic_write(target, b"TWO")

    assert len(seen) == 2
    assert seen[0] != seen[1], f"两次写的临时名不能相同：{seen}"
    assert all(str(os.getpid()) in name for name in seen), "临时名要带 pid，跨进程也不能撞"


@pytest.mark.skipif(os.name == "nt", reason="Windows 的 chmod 只切只读位，拿不到 0600")
def test_atomic_write_mode_applies_at_creation(tmp_path: Path) -> None:
    """私钥要在**创建时**就是 0600，而不是先 0644 再 chmod（有可读窗口）。"""
    target = tmp_path / "secret.bin"
    secure.atomic_write(target, b"x", mode=0o600)
    assert target.stat().st_mode & 0o777 == 0o600


def test_atomic_write_asks_for_mode_at_open(tmp_path: Path, monkeypatch) -> None:
    """光看最终权限不够：旧实现是「按 umask 建、再 chmod」，最终也是 0600。

    这里直接盯 `os.open` 的 mode 参数——只有「创建时就 0600」才没有可读窗口。
    """
    captured: dict[str, int] = {}
    real_open = secure.os.open

    def spy(path, flags, mode=0o777):
        captured["mode"] = mode
        return real_open(path, flags, mode)

    monkeypatch.setattr(secure.os, "open", spy)
    secure.atomic_write(tmp_path / "secret.bin", b"x", mode=0o600)
    assert captured.get("mode") == 0o600


def test_backup_master_key_reports_permission_warning(tmp_path: Path, monkeypatch) -> None:
    """权限收紧失败要把告警**返回**给调用方（界面负责显示），不能静默吞掉。"""
    monkeypatch.setattr(secure, "restrict_permissions", lambda _p: "权限没收紧")
    assert secure.backup_master_key("ab" * 32, tmp_path / "k.eekey", "pw") == "权限没收紧"


def test_backup_master_key_returns_none_when_permissions_ok(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.setattr(secure, "restrict_permissions", lambda _p: None)
    assert secure.backup_master_key("ab" * 32, tmp_path / "k.eekey", "pw") is None
