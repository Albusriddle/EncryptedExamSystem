"""启动入口接线：正常启动与 --dump-ui 共用含默认题库的新工程构造。"""

from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def test_launch_uses_default_project_constructor() -> None:
    src = (ROOT / "admin" / "app" / "main.py").read_text(encoding="utf-8")

    assert src.count("win = MainWindow()") == 2, "正常启动与诊断必须使用相同的默认工程"
    assert "MainWindow(autoload" not in src, "不再有 autoload 参数"
