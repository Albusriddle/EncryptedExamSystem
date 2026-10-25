"""启动入口的接线测试：正常启动必须自动打开上次工程。"""

from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def test_normal_launch_autoloads_but_dump_ui_does_not() -> None:
    """曾经的真实故障：正常启动也传了 autoload=False，自动打开功能等于没有。"""
    src = (ROOT / "admin" / "app" / "main.py").read_text(encoding="utf-8")

    assert src.count("MainWindow(autoload=False)") == 1, "只有 --dump-ui 该关自动打开"
    assert "win = MainWindow()" in src, "正常启动必须用默认构造（autoload=True）"
