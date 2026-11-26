"""新工程的示例题库；显式创建，不污染 ProjectState.load 或空状态对象。"""

from __future__ import annotations

import hashlib
import sys
import tempfile
from pathlib import Path

from app.core.bank_import import load_bank_from_path
from app.core.models import ExamBank
from app.core.paths import data_dir
from app.core.state import ProjectState


def demo_banks_dir() -> Path:
    # 冻结后不能再按源码层级找资源；打包时只附带公开的 banks/demo。
    base = getattr(sys, "_MEIPASS", None)
    root = Path(base) if base else Path(__file__).resolve().parents[3]
    return root / "banks" / "demo"


def _persistent_demo_path(bank: ExamBank, name: str) -> Path:
    # 工程只保存题库路径；_MEIPASS 在 AppImage 退出后会消失。按内容分目录，
    # 升级时新增快照、不覆盖旧工程引用的副本，也不复制整个教师资源目录。
    raw = bank.model_dump_json(indent=2).encode("utf-8")
    directory = data_dir() / "demo-banks" / hashlib.sha256(raw).hexdigest()
    path = directory / name
    tmp: Path | None = None
    try:
        directory.mkdir(parents=True, exist_ok=True)
        # 已有快照必须与当前资源逐字节一致；损坏/截断时重建，而不是一直复用坏文件。
        current = None
        if path.is_file():
            try:
                current = path.read_bytes()
            except OSError:
                current = None
        if current != raw:
            # 多个管理端可同时启动；唯一临时文件 + 原子替换避免读到半份 JSON。
            with tempfile.NamedTemporaryFile(dir=directory, delete=False) as stream:
                tmp = Path(stream.name)
                stream.write(raw)
            tmp.replace(path)
        return path.resolve()
    except OSError as exc:
        raise ValueError(
            f"无法保存示例题库到用户数据目录：{directory}。请检查目录权限或手动导入题库。"
        ) from exc
    finally:
        if tmp is not None:
            tmp.unlink(missing_ok=True)


def new_demo_project(*, persist: bool = True) -> ProjectState:
    """加载并验证示例；自检可读取原始资源而不创建持久快照。"""
    directory = demo_banks_dir()
    paths = sorted(
        path for path in directory.glob("*.json") if path.name != "manifest.example.json"
    )
    if not paths:
        raise ValueError(f"找不到自带示例题库：{directory}。请重新安装完整管理端或手动导入题库。")
    project = ProjectState()
    for path in paths:
        bank = load_bank_from_path(path)
        if persist and getattr(sys, "_MEIPASS", None):
            path = _persistent_demo_path(bank, path.name)
            bank = load_bank_from_path(path)
        if bank.exam_id in project.banks:
            raise ValueError(f"示例题库 ID 重复：{bank.exam_id}。请检查 {directory}。")
        project.add_bank(bank, str(path.resolve()))
    return project
