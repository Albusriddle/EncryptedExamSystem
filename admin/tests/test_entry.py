"""启动入口接线：正常启动与 --dump-ui 都使用空白工程（v0.9.0 起）。"""

from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def test_launch_uses_blank_project_constructor() -> None:
    src = (ROOT / "admin" / "app" / "main.py").read_text(encoding="utf-8")

    assert "win = MainWindow()" in src, "正常启动必须是空白工程"
    assert "MainWindow(autoload" not in src, "不再有 autoload 参数"
