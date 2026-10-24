"""发布包的许可证合规：Apache-2.0 §4(a) + PySide6 的 LGPL-3.0。"""

from __future__ import annotations

import importlib.util
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "build_exe", ROOT / "admin" / "buildtools" / "build_exe.py"
)


def _module():
    mod = importlib.util.module_from_spec(SPEC)
    assert SPEC.loader is not None
    SPEC.loader.exec_module(mod)
    return mod


def test_repo_has_license_and_notices() -> None:
    assert (ROOT / "LICENSE").is_file()
    assert (ROOT / "THIRD-PARTY-NOTICES.md").is_file()
    assert (ROOT / "licenses" / "LGPL-3.0.txt").is_file()
    assert (ROOT / "licenses" / "GPL-3.0.txt").is_file()
    assert "Apache License" in (ROOT / "LICENSE").read_text(encoding="utf-8")[:200]


def test_copy_legal_files_puts_them_in_release_dir() -> None:
    dest = ROOT / "target" / "test-legal"
    dest.mkdir(parents=True, exist_ok=True)
    copied = _module().copy_legal_files(dest)
    assert set(copied) == {"LICENSE", "THIRD-PARTY-NOTICES.md", "licenses/"}, copied
    assert (dest / "LICENSE").is_file()
    assert (dest / "licenses" / "LGPL-3.0.txt").is_file()
