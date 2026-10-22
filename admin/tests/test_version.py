"""版本号与水印的一致性检查。

版本号有三处体现：仓库根 `VERSION`、Cargo 工作区、admin/pyproject。
三者必须一致，否则会出现"程序里写 0.1.0、发布页写 0.1.4"这种尴尬。
Rust 端通过 include_str! 编译期读取 VERSION，Python 端运行时读取，
所以这里只需保证文件与项目元数据同步。
"""

from __future__ import annotations

import re
from pathlib import Path

from app.version import app_version, watermark

ROOT = Path(__file__).resolve().parents[2]


def _cargo_workspace_version() -> str:
    text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    sec = text.split("[workspace.package]", 1)[1]
    m = re.search(r'^version\s*=\s*"([^"]+)"', sec, re.MULTILINE)
    assert m, "Cargo.toml 未找到 workspace.package.version"
    return m.group(1)


def _admin_version() -> str:
    text = (ROOT / "admin" / "pyproject.toml").read_text(encoding="utf-8")
    m = re.search(r'^version\s*=\s*"([^"]+)"', text, re.MULTILINE)
    assert m, "admin/pyproject.toml 未找到 version"
    return m.group(1)


def test_version_file_exists() -> None:
    assert (ROOT / "VERSION").is_file()


def test_version_is_semver() -> None:
    v = (ROOT / "VERSION").read_text(encoding="utf-8").strip()
    assert re.fullmatch(r"\d+\.\d+\.\d+", v), f"版本号格式不合法: {v!r}"


def test_all_versions_agree() -> None:
    """VERSION 文件、Cargo 工作区、admin 包三处必须一致。"""
    file_v = (ROOT / "VERSION").read_text(encoding="utf-8").strip()
    assert file_v == _cargo_workspace_version(), "VERSION 与 Cargo.toml 不一致"
    assert file_v == _admin_version(), "VERSION 与 admin/pyproject.toml 不一致"


def test_app_version_reads_file() -> None:
    assert app_version() == (ROOT / "VERSION").read_text(encoding="utf-8").strip()


def test_watermark_format() -> None:
    w = watermark()
    assert w.startswith("EES v"), w
    assert w.endswith("Powered by AlbusRiddle"), w
    assert app_version() in w


def test_rust_reads_same_file() -> None:
    """Rust 端用 include_str! 编译期读取同一个 VERSION。"""
    lib = (ROOT / "crates" / "common" / "src" / "lib.rs").read_text(encoding="utf-8")
    assert 'include_str!("../../../VERSION")' in lib
    assert "Powered by AlbusRiddle" in lib
