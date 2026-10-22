"""builder.py 的参数拼装测试。

不调用真实 exam-builder（CI 上未必有 release 构建），
而是拦截 subprocess.run 检查命令行与环境变量——
这两点恰好是最容易出错、后果最严重的地方：

- master 一旦进了命令行，同机任何用户都能从进程列表读到；
- `--emit-keyfile` / `--no-embed-key` 拼错，教师拿到的包就用不了。
"""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from app.core import builder as bmod
from app.core.builder import BuilderError, build_package

MASTER = "ab" * 32  # 64 hex


class _FakeProc:
    def __init__(self, code: int = 0, out: str = "") -> None:
        self.returncode = code
        self.stdout = out
        self.stderr = ""


@pytest.fixture
def paths(tmp_path: Path) -> tuple[Path, Path, Path, Path]:
    builder_exe = tmp_path / "exam-builder.exe"
    builder_exe.write_bytes(b"fake")
    stub = tmp_path / "stub.exe"
    stub.write_bytes(b"fake")
    manifest = tmp_path / "manifest.json"
    manifest.write_text("{}", encoding="utf-8")
    banks = tmp_path / "banks"
    banks.mkdir()
    return builder_exe, manifest, banks, stub


@pytest.fixture
def captured(monkeypatch: pytest.MonkeyPatch) -> dict:
    box: dict = {}

    def fake_run(cmd, **kwargs):  # noqa: ANN001, ANN003
        box["cmd"] = cmd
        box["kwargs"] = kwargs
        # 输出要跟着参数走，否则测不出 --no-embed-key 的差异
        if "--no-embed-key" in cmd:
            return _FakeProc(0, "⚠ 未内嵌密钥（--no-embed-key）：学生必须自行提供 --master。")
        return _FakeProc(0, "✔ 已内嵌密钥：学生双击 exe 输入学号即可")

    monkeypatch.setattr(bmod.subprocess, "run", fake_run)
    return box


def test_master_never_on_command_line(paths, captured: dict) -> None:
    """master 只能走环境变量，绝不能出现 argv 里。"""
    builder_exe, manifest, banks, stub = paths
    build_package(builder_exe, manifest, banks, stub, stub, MASTER)
    cmd = captured["cmd"]
    joined = " ".join(str(c) for c in cmd)
    assert MASTER not in joined, "主密钥泄漏到命令行"
    assert captured["kwargs"]["env"]["EXAM_MASTER"] == MASTER


def test_default_embeds_key(paths, captured: dict) -> None:
    builder_exe, manifest, banks, stub = paths
    r = build_package(builder_exe, manifest, banks, stub, stub, MASTER)
    assert "--no-embed-key" not in captured["cmd"]
    assert r.embedded is True


def test_no_embed_key_flag(paths, captured: dict) -> None:
    builder_exe, manifest, banks, stub = paths
    r = build_package(builder_exe, manifest, banks, stub, stub, MASTER, embed_key=False)
    assert "--no-embed-key" in captured["cmd"]
    # 输出里没有"已内嵌密钥"，结果对象应如实反映
    assert r.embedded is False


def test_emit_keyfile_flag(paths, captured: dict) -> None:
    builder_exe, manifest, banks, stub = paths
    kf = stub.parent / "kf.txt"
    r = build_package(builder_exe, manifest, banks, stub, stub, MASTER, emit_keyfile=kf)
    assert "--emit-keyfile" in captured["cmd"]
    assert str(kf) in captured["cmd"]
    assert r.keyfile == kf


def test_no_keyfile_flag_when_not_requested(paths, captured: dict) -> None:
    builder_exe, manifest, banks, stub = paths
    r = build_package(builder_exe, manifest, banks, stub, stub, MASTER)
    assert "--emit-keyfile" not in captured["cmd"]
    assert r.keyfile is None


def test_output_decoded_as_utf8(paths, captured: dict) -> None:
    """Windows 上必须显式 UTF-8，否则解码 Rust 输出的中文会崩。"""
    builder_exe, manifest, banks, stub = paths
    build_package(builder_exe, manifest, banks, stub, stub, MASTER)
    assert captured["kwargs"]["encoding"] == "utf-8"
    assert captured["kwargs"]["errors"] == "replace"


def test_missing_builder_raises(tmp_path: Path) -> None:
    with pytest.raises(BuilderError, match="找不到 builder"):
        build_package(
            tmp_path / "nope.exe",
            tmp_path / "m.json",
            tmp_path,
            tmp_path / "s.exe",
            tmp_path / "o.exe",
            MASTER,
        )


def test_bad_master_length_raises(paths) -> None:
    builder_exe, manifest, banks, stub = paths
    with pytest.raises(BuilderError, match="64 位十六进制"):
        build_package(builder_exe, manifest, banks, stub, stub, "abc")


def test_failure_reports_not_ok(paths, monkeypatch: pytest.MonkeyPatch) -> None:
    builder_exe, manifest, banks, stub = paths
    monkeypatch.setattr(bmod.subprocess, "run", lambda *a, **k: _FakeProc(2, ""))
    r = build_package(builder_exe, manifest, banks, stub, stub, MASTER)
    assert r.ok is False
    assert r.package is None
    assert r.embedded is False


def test_timeout_propagates(paths, monkeypatch: pytest.MonkeyPatch) -> None:
    """打包超时不应被静默吞掉。"""

    def boom(*a, **k):  # noqa: ANN002, ANN003
        raise subprocess.TimeoutExpired(cmd="exam-builder", timeout=1)

    monkeypatch.setattr(bmod.subprocess, "run", boom)
    builder_exe, manifest, banks, stub = paths
    with pytest.raises(subprocess.TimeoutExpired):
        build_package(builder_exe, manifest, banks, stub, stub, MASTER)
