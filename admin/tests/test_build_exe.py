"""`build_exe.py` 的版本比对与学生端模板检查（纯函数，不跑 PyInstaller）。

为什么单独测：这两个判断原本是「打印一行、不比对」「找不到就静默跳过」，
正是它们让发布包可能嵌进旧二进制、或缺 `stub-template.exe` 而没人发现。
"""

from __future__ import annotations

from pathlib import Path

import pytest

from buildtools.build_exe import _version_matches, check_client_template, pick_artifact_with_version


@pytest.mark.parametrize(
    ("output", "want", "ok"),
    [
        ("exam-builder 1.4.8\n", "1.4.8", True),
        ("exam-server 1.4.8", "1.4.8", True),
        ("  exam-builder   1.4.8  ", "1.4.8", True),
        # `1.4.8` 是 `1.4.80` 的子串：用 `in` 判断会把旧版本判成匹配
        ("exam-builder 1.4.80", "1.4.8", False),
        ("exam-builder 1.4.7", "1.4.8", False),
        ("exam-builder", "1.4.8", False),
        ("", "1.4.8", False),
        ("garbage", "1.4.8", False),
        # semver 的 `v` 前缀与 `+build` 构建元数据不影响版本身份
        ("exam-builder v1.4.8", "1.4.8", True),
        ("exam-builder 1.4.8+build.7", "1.4.8", True),
        ("exam-builder v1.4.8+build.7", "1.4.8", True),
        ("exam-builder 1.4.8", "v1.4.8", True),
        ("exam-builder v1.4.80", "1.4.8", False),
    ],
)
def test_version_matches(output: str, want: str, ok: bool) -> None:
    assert _version_matches(output, want) is ok


def test_client_template_missing_is_an_error() -> None:
    """找不到学生端模板必须报错，而不是静默跳过。

    静默跳过时发布包里没有 `stub-template.exe`，失败点被推到
    `make-release-zip.sh`——报错位置离原因很远。
    """
    message = check_client_template(None)
    assert message is not None
    assert "exam-client" in message


def test_client_template_present_is_ok(tmp_path: Path) -> None:
    exe = tmp_path / "exam-client.exe"
    exe.write_bytes(b"MZ")
    assert check_client_template(exe) is None


# ---------------------------------------------------------------- 候选回退


def test_pick_artifact_tries_every_candidate(tmp_path: Path, monkeypatch) -> None:
    """第一个候选版本不对时要换下一个，而不是直接判失败。

    为什么要遍历：`find_artifacts` 会同时看到 `target/release` 与
    `target/<triple>/release`；`rust-cache` 恢复的旧产物 mtime 可能更新而排在前面。
    只看第一个会把「选错产物」变成硬失败（或更糟：静默嵌旧二进制）。
    """
    from buildtools import build_exe

    first = tmp_path / "target" / "release" / "exam-builder"
    second = tmp_path / "target" / "x86_64-pc-windows-msvc" / "release" / "exam-builder"
    for path in (first, second):
        path.parent.mkdir(parents=True)
        path.write_bytes(b"MZ")

    monkeypatch.setattr(build_exe, "find_artifacts", lambda _name, _root=None: [first, second])
    checked: list[Path] = []

    def fake_require(_label: str, exe: Path, _want: str) -> bool:
        checked.append(exe)
        return exe == second

    monkeypatch.setattr(build_exe, "_require_artifact_version", fake_require)

    assert pick_artifact_with_version("exam-builder", "exam-builder", "1.4.8") == second
    assert checked == [first, second], "必须按顺序把两个候选都试过"


def test_pick_artifact_reports_when_no_candidate_matches(monkeypatch, capsys) -> None:
    from buildtools import build_exe

    monkeypatch.setattr(build_exe, "find_artifacts", lambda *_a, **_k: [Path("a"), Path("b")])
    monkeypatch.setattr(build_exe, "_require_artifact_version", lambda *_a, **_k: False)

    assert pick_artifact_with_version("exam-builder", "exam-builder", "1.4.8") is None
    out = capsys.readouterr().out
    assert "1.4.8" in out and "a" in out, "失败时要把期望版本与候选路径都打出来"


def test_pick_artifact_missing_is_reported(monkeypatch, capsys) -> None:
    from buildtools import build_exe

    monkeypatch.setattr(build_exe, "find_artifacts", lambda *_a, **_k: [])
    assert pick_artifact_with_version("exam-builder", "exam-builder", "1.4.8") is None
    assert "cargo build" in capsys.readouterr().out


# ---------------------------------------------------------------- 许可证复制


def _legal_root(tmp_path: Path, *, with_isc: bool = True) -> Path:
    """造一个完整许可夹具；只有指定的 ISC 缺失，不能被其它缺件掩盖。"""
    root = tmp_path / "repo"
    (root / "licenses").mkdir(parents=True)
    (root / "LICENSE").write_text("Apache License 2.0\n", encoding="utf-8")
    (root / "THIRD-PARTY-NOTICES.md").write_text("第三方声明\n", encoding="utf-8")
    for name in (
        "LGPL-3.0.txt",
        "GPL-3.0.txt",
        "OFL-1.1-Noto.txt",
        "AppImage-runtime.txt",
        "LGPL-2.1.txt",
        "AppImage-musl.txt",
        "AppImage-squashfuse.txt",
        "AppImage-zstd.txt",
        "AppImage-zlib.txt",
    ):
        (root / "licenses" / name).write_text(f"{name}\n", encoding="utf-8")
    if with_isc:
        (root / "licenses" / "ISC.txt").write_text(
            "Copyright 2015-2025 Brian Smith.\n", encoding="utf-8"
        )
    return root


def test_copy_legal_files_includes_isc(tmp_path: Path) -> None:
    """`ring` 是 Apache-2.0 AND ISC：发布目录必须带上 `licenses/ISC.txt`。"""
    from buildtools.build_exe import copy_legal_files

    root = _legal_root(tmp_path)
    dest = tmp_path / "dist" / "ExamAdmin"
    dest.mkdir(parents=True)

    copied = copy_legal_files(dest, root=root)

    assert (dest / "licenses" / "ISC.txt").is_file(), "缺 ISC.txt 等于发布包缺许可"
    assert "licenses/" in copied
    for source in (root / "licenses").iterdir():
        assert (dest / "licenses" / source.name).read_bytes() == source.read_bytes()


def test_copy_legal_files_rejects_missing_isc(tmp_path: Path) -> None:
    """缺 ISC.txt 时必须**构建失败**，而不是「有就拷、没有就跳过」。"""
    from buildtools.build_exe import copy_legal_files

    root = _legal_root(tmp_path, with_isc=False)
    dest = tmp_path / "dist" / "ExamAdmin"
    dest.mkdir(parents=True)

    with pytest.raises(FileNotFoundError, match="ISC.txt"):
        copy_legal_files(dest, root=root)
