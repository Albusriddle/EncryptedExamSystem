"""管理端打包相关逻辑的测试。

覆盖三块（都刻意与 Qt 解耦，CI 无 GUI 依赖也能跑）：

1. `app/core/buildcmd` —— 命令行拼装：master 必须在环境变量里、`--force` 必须显式；
2. `app/core/banks_io` —— 临时题库必须剥掉标准答案；
3. `app/ui/tab_build` —— 路径推断（需 PySide6，缺依赖时跳过本组）。
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

from app.core.buildcmd import MASTER_ENV, build_command, build_env

MASTER = "ab" * 32  # 64 hex

# stub 文件名与平台相关（tab_build.default_stub 用 os.name 判断）。
# 测试若写死 ".exe"，在 Linux CI 上必然失败——这正是真跑 UI 测试后才暴露的。
STUB_NAME = "stub-template.exe" if os.name == "nt" else "stub-template"


def _paths(tmp_path: Path) -> dict[str, Path]:
    return {
        "builder_exe": tmp_path / "exam-builder.exe",
        "manifest": tmp_path / "manifest.json",
        "banks_dir": tmp_path / "banks",
        "stub": tmp_path / "stub.exe",
        "out": tmp_path / "out.exe",
    }


# ---------------------------------------------------------------- 命令行拼装


def test_master_never_on_command_line(tmp_path: Path) -> None:
    """master 只能走环境变量，绝不能出现在 argv 里。"""
    p = _paths(tmp_path)
    cmd = build_command(**p)
    assert MASTER not in " ".join(cmd), "主密钥泄漏到命令行"
    env = build_env(MASTER, base={})
    assert env[MASTER_ENV] == MASTER


def test_force_is_always_passed_by_admin(tmp_path: Path) -> None:
    """管理端必须显式 --force：builder 默认拒绝覆盖（覆盖确认在界面上做过）。"""
    p = _paths(tmp_path)
    assert "--force" in build_command(**p)
    assert "--force" not in build_command(**p, force=False)


def test_emit_keyfile_only_when_requested(tmp_path: Path) -> None:
    p = _paths(tmp_path)
    kf = tmp_path / "kf.txt"
    with_kf = build_command(**p, keyfile=kf)
    assert "--emit-keyfile" in with_kf and str(kf) in with_kf
    assert "--emit-keyfile" not in build_command(**p)


def test_paths_are_passed_through(tmp_path: Path) -> None:
    p = _paths(tmp_path)
    cmd = build_command(**p)
    for key in ("manifest", "banks_dir", "stub", "out"):
        assert str(p[key]) in cmd, f"{key} 未传给 builder"


def test_no_embed_key_not_used_by_admin(tmp_path: Path) -> None:
    """管理端永远要内嵌密钥（学生零配置）。"""
    assert "--no-embed-key" not in build_command(**_paths(tmp_path))


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
    # exclude_none 之后连键都不写，比"值为 null"更彻底
    assert "answer" not in obj["questions"][0], "临时题库里仍带答案"
    assert "judge" not in obj["questions"][0], "临时题库里仍带判分材料"

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

    tab_build = _tab_build()

    exe_dir = tmp_path / "ExamAdmin"
    exe_dir.mkdir()
    stub = exe_dir / STUB_NAME
    stub.write_bytes(b"x")

    monkeypatch.setattr(sys, "frozen", True, raising=False)
    monkeypatch.setattr(sys, "executable", str(exe_dir / "ExamAdmin.exe"))

    assert tab_build.default_stub() == stub


def test_default_stub_frozen_missing_returns_expected_location(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """stub 不存在时，返回"应该放的位置"，便于报错里给出指引。"""
    import sys

    tab_build = _tab_build()

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

    tab_build = _tab_build()

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

    tab_build = _tab_build()

    monkeypatch.setattr(sys, "frozen", False, raising=False)
    assert "target" in str(tab_build.default_stub())
    assert tab_build.default_out().name == "StudentClient.exe"


def test_default_out_skips_unwritable_dirs(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """默认输出目录必须真的可写。

    硬编码桌面踩过坑：某些环境桌面被 OneDrive 接管，
    打包报「拒绝访问 (os error 5)」。
    """
    import sys

    tab_build = _tab_build()

    home = tmp_path / "home"
    docs = home / "Documents"
    docs.mkdir(parents=True)
    monkeypatch.setattr(sys, "frozen", True, raising=False)
    monkeypatch.setattr(sys, "executable", str(tmp_path / "app" / "ExamAdmin.exe"))
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: home))

    out = tab_build.default_out()
    assert tab_build._writable(out.parent), "默认输出目录必须可写"


def test_writable_detects_readonly_dir(tmp_path: Path) -> None:
    _writable = _tab_build()._writable

    ok = tmp_path / "ok"
    ok.mkdir()
    assert _writable(ok) is True

    # 指向一个"文件"当目录 → 必然不可写
    not_dir = tmp_path / "file.txt"
    not_dir.write_text("x", encoding="utf-8")
    assert _writable(not_dir) is False


def _tab_build():
    """按需导入界面模块。

    这些用例测的是 `tab_build` 里的路径推断，必须有 PySide6。
    保留 importorskip 是为了"没装 GUI 依赖的环境也能跑完其余测试"
    （CI 现在装了 PySide6，这些用例会真正执行）。
    """
    pytest.importorskip("PySide6")
    from app.ui import tab_build  # noqa: PLC0415 - 延迟导入，缺 GUI 依赖时只跳过本组用例

    return tab_build


def test_pin_login_flags(tmp_path: Path) -> None:
    p = _paths(tmp_path)
    pins = tmp_path / "pins.csv"
    cmd = build_command(**p, pin_login=True, emit_pins=pins)
    assert "--pin-login" in cmd
    assert cmd[cmd.index("--emit-pins") + 1] == str(pins)
