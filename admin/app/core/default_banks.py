"""新工程的示例题库；显式创建，不污染 ProjectState.load 或空状态对象。"""

from __future__ import annotations

import sys
from pathlib import Path

from app.core.bank_import import load_bank_from_path
from app.core.state import ProjectState


def demo_banks_dir() -> Path:
    # 冻结后不能再按源码层级找资源；打包时只附带公开的 banks/demo。
    base = getattr(sys, "_MEIPASS", None)
    root = Path(base) if base else Path(__file__).resolve().parents[3]
    return root / "banks" / "demo"


def new_demo_project() -> ProjectState:
    directory = demo_banks_dir()
    paths = sorted(
        path for path in directory.glob("*.json") if path.name != "manifest.example.json"
    )
    if not paths:
        raise ValueError(f"找不到自带示例题库：{directory}。请重新安装完整管理端或手动导入题库。")
    project = ProjectState()
    for path in paths:
        bank = load_bank_from_path(path)
        if bank.exam_id in project.banks:
            raise ValueError(f"示例题库 ID 重复：{bank.exam_id}。请检查 {directory}。")
        project.add_bank(bank, str(path.resolve()))
    return project
