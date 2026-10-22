"""欢迎弹窗与品牌信息的回归测试。

重点不是"弹窗能弹出来"，而是：
- 文案里必须出现仓库地址、许可证、三位致谢对象；
- 声明的许可证必须与仓库 LICENSE 文件一致（曾经 Cargo.toml 写 MIT、
  LICENSE 是 Apache-2.0，两边打架）；
- 「不再提示」要真的记住。
"""

from __future__ import annotations

from pathlib import Path

import pytest

pytest.importorskip("PySide6")

from PySide6.QtCore import QSettings  # noqa: E402
from PySide6.QtWidgets import QApplication  # noqa: E402

from app.branding import branding  # noqa: E402
from app.ui import welcome as w  # noqa: E402
from app.version import app_version  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]


@pytest.fixture(scope="module")
def qapp():
    import sys

    app = QApplication.instance() or QApplication(sys.argv)
    yield app


def test_rich_text_contains_everything_required() -> None:
    text = w._rich_text()
    b = branding()
    assert b.repo_url in text, "缺少仓库地址"
    assert b.license_name in text, "缺少许可证名称"
    assert b.license in text, "缺少许可证标识"
    for c in b.credits:
        assert c.name in text, f"缺少致谢对象 {c.name}"
        assert c.url in text, f"缺少致谢链接 {c.url}"


def test_rich_text_shows_version_and_welcome() -> None:
    text = w._rich_text()
    assert app_version() in text, "缺少版本号"
    assert "欢迎使用" in text


def test_star_hint_is_present() -> None:
    assert "Star" in w._rich_text() or "star" in w._rich_text()


def test_license_consistent_with_license_file() -> None:
    """弹窗声明的许可证必须与 LICENSE 文件一致。"""
    license_text = (ROOT / "LICENSE").read_text(encoding="utf-8")
    b = branding()
    if "Apache License" in license_text:
        assert "Apache" in b.license_name
        assert b.license == "Apache-2.0"


def test_dialog_builds_and_can_remember(qapp) -> None:
    dlg = w.WelcomeDialog()
    assert dlg.windowTitle() == "欢迎"
    assert dlg.should_remember() is False
    dlg.dont_show.setChecked(True)
    assert dlg.should_remember() is True
    dlg.deleteLater()


def test_dismiss_is_persisted(qapp, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """勾选「不再提示」后，下次启动不再弹。"""
    monkeypatch.setenv("LOCALAPPDATA", str(tmp_path))
    monkeypatch.setattr(w.WelcomeDialog, "exec", lambda self: 1)

    w.show_welcome_if_needed()
    assert w._settings().value(w._SETTINGS_KEY, False, type=bool) is False, "未勾选时不应记住"

    monkeypatch.setattr(w.WelcomeDialog, "should_remember", lambda self: True)
    w.show_welcome_if_needed()
    settings = w._settings()
    assert settings.status() == QSettings.Status.NoError
    assert settings.value(w._SETTINGS_KEY, False, type=bool) is True, "勾选后应记住"
    assert w.settings_path().is_file(), "应落盘到 settings.ini"


def test_settings_path_is_under_localappdata(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """不能用注册表后端：受限账户下会 AccessError，「不再提示」存不住。"""
    monkeypatch.setenv("LOCALAPPDATA", str(tmp_path))
    p = w.settings_path()
    assert p.name == "settings.ini"
    assert p.parent.name == "ExamSystem"


def test_main_wires_the_welcome_dialog() -> None:
    """启动入口必须真的调用欢迎弹窗。

    曾经出现过：welcome.py 写好了、测试也过了，但 main() 里那行调用
    因为一次代码格式化被悄悄删掉——教师端于是完全没有弹窗，
    而所有单元测试依然全绿。这条"接线测试"专门守这个洞。
    """
    src = (ROOT / "admin" / "app" / "main.py").read_text(encoding="utf-8")
    assert "show_welcome_if_needed" in src, "main.py 未调用欢迎弹窗"
    # 必须传主窗口作为父级，否则弹窗可能跑到主窗口后面
    assert "show_welcome_if_needed(win)" in src, "调用时未传主窗口"


def test_credit_role_is_founder_and_author() -> None:
    """创始人一栏的措辞应为「创始人/仓库作者」。"""
    roles = [c.role for c in branding().credits]
    assert "创始人/仓库作者" in roles, f"当前角色: {roles}"
    assert "创始人" not in roles, "不应保留旧措辞"
