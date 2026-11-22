"""网页考试命令拼装与校验。"""

from pathlib import Path

from app.core.webexam import access_url, validate_inputs, web_command


def test_web_command_passes_package_and_keyfile_only(tmp_path: Path) -> None:
    cmd = web_command(
        server=Path("exam-server.exe"),
        package=tmp_path / "pkg.exe",
        keyfile=tmp_path / "key.txt",
        port=8080,
        answers_dir=tmp_path / "answers",
    )
    assert cmd[0].endswith("exam-server.exe")
    assert "--package" in cmd and "--keyfile" in cmd
    assert "--master" not in cmd, "网页考试不应把 master 传给服务端"
    assert cmd[cmd.index("--port") + 1] == "8080"


def test_validate_inputs(tmp_path: Path) -> None:
    pkg = tmp_path / "pkg.exe"
    key = tmp_path / "key.txt"
    assert validate_inputs(pkg, key, 8080) is not None
    pkg.write_bytes(b"MZ")
    assert validate_inputs(pkg, key, 8080) is not None
    key.write_text("00" * 32, encoding="utf-8")
    assert validate_inputs(pkg, key, 80) is not None
    assert validate_inputs(pkg, key, 8080) is None


def test_access_url_uses_given_ip() -> None:
    assert access_url(8080, "192.168.1.5") == "http://192.168.1.5:8080/"
