from pathlib import Path

from app.ui.tab_build import BuildTab


def test_pin_target_rejects_output_directory(tmp_path: Path) -> None:
    out = tmp_path / "StudentClient.exe"
    out.write_bytes(b"x")
    assert BuildTab._pin_target_error(out, tmp_path / "PIN.csv")


def test_pin_target_accepts_separate_directory(tmp_path: Path) -> None:
    out = tmp_path / "out" / "StudentClient.exe"
    out.parent.mkdir()
    out.write_bytes(b"x")
    safe = tmp_path / "safe" / "PIN.csv"
    assert BuildTab._pin_target_error(out, safe) is None
