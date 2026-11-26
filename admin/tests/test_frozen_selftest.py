"""打包版自检要诊断随包资源，不应在 Windows 或 Linux 写用户数据。"""

from __future__ import annotations

import shutil
import sys
from pathlib import Path

import pytest

pytest.importorskip("PySide6")


@pytest.mark.parametrize("intact", [True, False], ids=["intact", "corrupt"])
def test_frozen_selftest_validates_demo_without_writing_user_data(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    intact: bool,
) -> None:
    from app import main as entry

    root = Path(__file__).resolve().parents[2]
    resources = tmp_path / "frozen" / "_internal"
    demo = resources / "banks" / "demo"
    shutil.copytree(root / "banks" / "demo", demo)
    shutil.copy2(root / "branding.json", resources)
    if not intact:
        (demo / "math.json").write_text("not JSON", encoding="utf-8")
    user_data = tmp_path / "profile"
    monkeypatch.setattr(sys, "_MEIPASS", str(resources), raising=False)
    monkeypatch.setenv("LOCALAPPDATA", str(user_data))
    monkeypatch.setattr(sys, "argv", ["ExamAdmin", "--selftest"])

    assert entry.main() == (0 if intact else 1)
    if not intact:
        assert "math.json" in capsys.readouterr().err
    assert not user_data.exists()
