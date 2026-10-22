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


# ---------------------------------------------------------------- 答案泄漏防线


def test_temp_banks_have_no_answers(tmp_path: Path) -> None:
    """打包用的临时题库**绝不能**带 answer。

    旧实现直接 model_dump_json 落盘，把标准答案留在 %TEMP% 里；
    且打包结束从不删除目录。这条测试守住"写出去的文件不含答案"。
    """
    import json

    from app.core.banks_io import write_temp_banks
    from app.core.models import ExamBank

    bank = ExamBank.model_validate(
        {
            "exam_id": "math",
            "title": "数学",
            "pick": 1,
            "questions": [
                {
                    "id": "q1",
                    "type": "single",
                    "stem": "1+1=?",
                    "options": ["1", "2"],
                    "answer": "B",
                    "score": 2,
                }
            ],
        }
    )
    write_temp_banks({"math": bank}, tmp_path)

    text = (tmp_path / "math.json").read_text(encoding="utf-8")
    obj = json.loads(text)
    assert obj["questions"][0]["answer"] is None, "临时题库里仍带答案"

    # 源对象不能被就地改动（必须 deep copy）
    assert bank.questions[0].answer == "B"


# ---------------------------------------------------------------- 产物定位


def _touch(path: Path) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(b"x")
    return path


def test_find_artifact_local_layout(tmp_path: Path) -> None:
    """本机 `cargo build --release` 的布局。"""
    from buildtools.build_exe import find_artifact

    want = _touch(tmp_path / "target" / "release" / "exam-builder.exe")
    assert find_artifact("exam-builder.exe", root=tmp_path) == want


def test_find_artifact_ci_layout(tmp_path: Path) -> None:
    """CI `cargo build --release --target <triple>` 的布局。

    这是 release 工作流真实使用的布局。旧实现只认 target/release，
    在全新 checkout（无旧残留）下必然找不到而失败。
    """
    from buildtools.build_exe import find_artifact

    want = _touch(tmp_path / "target" / "x86_64-pc-windows-msvc" / "release" / "exam-builder.exe")
    (tmp_path / "target" / "release").mkdir(parents=True, exist_ok=True)
    assert find_artifact("exam-builder.exe", root=tmp_path) == want


def test_find_artifact_prefers_local(tmp_path: Path) -> None:
    """两者都在时优先 target/release（本机开发时的常见情形）。"""
    from buildtools.build_exe import find_artifact

    local = _touch(tmp_path / "target" / "release" / "exam-builder.exe")
    _touch(tmp_path / "target" / "x86_64-pc-windows-msvc" / "release" / "exam-builder.exe")
    assert find_artifact("exam-builder.exe", root=tmp_path) == local


def test_find_artifact_missing_returns_none(tmp_path: Path) -> None:
    from buildtools.build_exe import find_artifact

    (tmp_path / "target").mkdir()
    assert find_artifact("exam-builder.exe", root=tmp_path) is None


# ---------------------------------------------------------------- 冻结环境路径


def test_default_stub_uses_program_dir_when_frozen(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """打包后必须去 ExamAdmin.exe 旁边找 stub-template.exe。

    用户实测：旧实现按源码目录结构拼出
    .../ExamAdmin/target/release/exam-client.exe，点打包直接报"找不到 stub"。
    """
    import sys

    from app.ui import tab_build

    exe_dir = tmp_path / "ExamAdmin"
    exe_dir.mkdir()
    stub = exe_dir / "stub-template.exe"
    stub.write_bytes(b"x")

    monkeypatch.setattr(sys, "frozen", True, raising=False)
    monkeypatch.setattr(sys, "executable", str(exe_dir / "ExamAdmin.exe"))

    assert tab_build.default_stub() == stub


def test_default_stub_frozen_missing_returns_expected_location(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """stub 不存在时，返回"应该放的位置"，便于报错里给出指引。"""
    import sys

    from app.ui import tab_build

    exe_dir = tmp_path / "ExamAdmin"
    exe_dir.mkdir()
    monkeypatch.setattr(sys, "frozen", True, raising=False)
    monkeypatch.setattr(sys, "executable", str(exe_dir / "ExamAdmin.exe"))

    got = tab_build.default_stub()
    assert got.parent == exe_dir
    assert got.name.startswith("stub-template")


def test_default_out_when_frozen_is_writable_user_dir(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """输出不能落在程序目录（可能在 Program Files）。"""
    import sys

    from app.ui import tab_build

    home = tmp_path / "home"
    desktop = home / "Desktop"
    desktop.mkdir(parents=True)
    monkeypatch.setattr(sys, "frozen", True, raising=False)
    monkeypatch.setattr(sys, "executable", str(tmp_path / "ExamAdmin" / "ExamAdmin.exe"))
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: home))

    out = tab_build.default_out()
    assert out.parent == desktop
    assert out.name.endswith(".exe")


def test_default_paths_not_frozen_use_repo_layout(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """源码运行时仍指向仓库的 target/release 与 dist。"""
    import sys

    from app.ui import tab_build

    monkeypatch.setattr(sys, "frozen", False, raising=False)
    assert "target" in str(tab_build.default_stub())
    assert tab_build.default_out().name == "StudentClient.exe"


def test_default_out_skips_unwritable_dirs(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """默认输出目录必须真的可写。

    硬编码桌面踩过坑：某些环境桌面被 OneDrive 接管，
    打包报「拒绝访问 (os error 5)」。
    """
    import sys

    from app.ui import tab_build

    home = tmp_path / "home"
    docs = home / "Documents"
    docs.mkdir(parents=True)
    monkeypatch.setattr(sys, "frozen", True, raising=False)
    monkeypatch.setattr(sys, "executable", str(tmp_path / "app" / "ExamAdmin.exe"))
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: home))

    out = tab_build.default_out()
    assert tab_build._writable(out.parent), "默认输出目录必须可写"


def test_writable_detects_readonly_dir(tmp_path: Path) -> None:
    from app.ui.tab_build import _writable

    ok = tmp_path / "ok"
    ok.mkdir()
    assert _writable(ok) is True

    # 指向一个"文件"当目录 → 必然不可写
    not_dir = tmp_path / "file.txt"
    not_dir.write_text("x", encoding="utf-8")
    assert _writable(not_dir) is False
