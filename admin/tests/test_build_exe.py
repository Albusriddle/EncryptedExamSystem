"""`build_exe.py` 的版本比对与学生端模板检查（纯函数，不跑 PyInstaller）。

为什么单独测：这两个判断原本是「打印一行、不比对」「找不到就静默跳过」，
正是它们让发布包可能嵌进旧二进制、或缺 `stub-template.exe` 而没人发现。
"""

from __future__ import annotations

from pathlib import Path

import pytest

from buildtools.build_exe import _version_matches, check_client_template


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
