"""发版版本号脚本：只改标记位，不动历史叙述。

为什么值得测：这个脚本就是为了替代「全库替换版本号」——那种做法会把
`v1.2.3 起`（能力引入版本）、`## 更新摘要（v1.2.2）`（历史发版说明）、
`第一百轮（…，v1.2.2）`（审阅记录）一起改掉。用例里刻意放了这些历史行，
只要脚本越界，断言立刻红。
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "bump_version.py"

OLD = "1.2.3"
NEW = "1.3.0"

FILES = {
    "VERSION": f"{OLD}\n",
    "Cargo.toml": f'[workspace]\nmembers = []\n\n[workspace.package]\nversion = "{OLD}"\n',
    "admin/pyproject.toml": f'[project]\nname = "exam-admin"\nversion = "{OLD}"\n',
    "admin/uv.lock": (
        f'[[package]]\nname = "exam-admin"\nversion = "{OLD}"\nsource = {{ virtual = "." }}\n'
    ),
    "Cargo.lock": "".join(
        f'[[package]]\nname = "{crate}"\nversion = "{OLD}"\n\n'
        for crate in ("builder", "client", "common", "server")
    ),
    "README.md": (
        f"当前版本：**{OLD}**（`VERSION` / `Cargo.toml` / `admin/pyproject.toml` 三处一致）\n"
        "\n其它说明（这段不能被改）。\n"
        f"\n## 8. 测试与验证基线（v{OLD}）\n"
        "\n| Rust | 100 个 `#[test]` |\n"
    ),
    "docs/保密考试系统_架构与实施方案.md": (
        f"> 适用版本：**v{OLD}**。\n"
        "\n```bash\n"
        f"cargo test --workspace                     # v{OLD}：100 个 #[test]\n"
        f"cd admin && uv run pytest -q               # v{OLD}：200 用例（含参数化展开）\n"
        "```\n"
    ),
    "docs/代码审阅报告.md": (
        f"## 当前基线（v{OLD}）\n"
        "\n| Rust | **100** passed |\n"
        "\n## 第一百轮（2024-01-01，v1.2.2）\n"
        "\n| 1 | 旧问题 | `v1.2.2 起` 修好 |\n"
    ),
    "docs/release-notes.md": (
        f"> 当前版本 **{OLD}**。Release 正文由 `scripts/release_body.py` 从本文件提取\n"
        "> **记录范围**：共 2 个版本，最新在前。\n"
        "\n"
        f"## 更新摘要（v{NEW}）\n"
        "\n- 新版本内容（发版前由人工写）。\n"
        "\n"
        f"## 更新摘要（v{OLD}）\n"
        f"\n- `v{OLD} 起` 支持某能力。\n"
        "\n"
        "## 更新摘要（v1.2.2）\n"
        "\n- 更早的版本。\n"
    ),
    "docs/出题质检与难度口径.md": f"> 适用版本：**v{OLD}**。口径变了必须同时改工具。\n",
    "docs/时间令牌协议.md": f"> 适用版本 **v{OLD}**；容器门禁见另一篇。\n",
    "docs/图片使用教程.md": f"- 适用版本：**v{OLD}**（图片功能自 v0.5.0 起）\n",
    "docs/题库JSON规范.md": f"> 适用版本：**v{OLD}**。格式变化必须同步 schema。\n",
    "docs/安全边界与威胁模型.md": f"> 适用版本：**v{OLD}**。本文是安全口径的唯一来源。\n",
    "docs/教师使用手册.md": f"- 适用版本：**v{OLD}**\n",
}


def make_tree(tmp_path: Path, *, with_new_section: bool = True) -> Path:
    for rel, text in FILES.items():
        if not with_new_section and rel == "docs/release-notes.md":
            text = text.replace(
                f"## 更新摘要（v{NEW}）\n\n- 新版本内容（发版前由人工写）。\n\n", ""
            )
        path = tmp_path / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8", newline="")
    return tmp_path


def run(root: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(SCRIPT), *args, "--root", str(root)],
        capture_output=True,
        encoding="utf-8",
        errors="replace",
        timeout=60,
        env={**os.environ, "PYTHONIOENCODING": "utf-8"},
    )


def read(root: Path, rel: str) -> str:
    return (root / rel).read_text(encoding="utf-8")


def test_bumps_only_markers_and_keeps_history(tmp_path: Path) -> None:
    root = make_tree(tmp_path)
    result = run(root, NEW)
    assert result.returncode == 0, result.stderr

    assert read(root, "VERSION") == f"{NEW}\n"
    for rel in ("Cargo.toml", "admin/pyproject.toml", "admin/uv.lock", "Cargo.lock"):
        assert NEW in read(root, rel), f"{rel} 没跟着改"
        assert f'"{OLD}"' not in read(root, rel), f"{rel} 还留着旧版本"

    readme = read(root, "README.md")
    assert f"当前版本：**{NEW}**" in readme
    assert f"## 8. 测试与验证基线（v{NEW}）" in readme

    arch = read(root, "docs/保密考试系统_架构与实施方案.md")
    assert f"> 适用版本：**v{NEW}**。" in arch
    assert f"# v{NEW}：100 个 #[test]" in arch, "只改版本，不动测试计数"
    assert f"# v{NEW}：200 用例" in arch

    review = read(root, "docs/代码审阅报告.md")
    assert f"## 当前基线（v{NEW}）" in review
    # 历史轮次不能被碰
    assert "## 第一百轮（2024-01-01，v1.2.2）" in review
    assert "`v1.2.2 起` 修好" in review

    notes = read(root, "docs/release-notes.md")
    assert f"> 当前版本 **{NEW}**。" in notes
    assert "共 3 个版本" in notes, "版本计数按小节数自动算"
    assert f"## 更新摘要（v{OLD}）" in notes, "历史小节不能被改"
    assert "## 更新摘要（v1.2.2）" in notes
    assert f"- `v{OLD} 起` 支持某能力。" in notes, "正文里的历史版本叙述不能被改"

    assert f"> 适用版本：**v{NEW}**。" in read(root, "docs/出题质检与难度口径.md")
    assert f"> 适用版本 **v{NEW}**；" in read(root, "docs/时间令牌协议.md")


def test_check_passes_after_bump(tmp_path: Path) -> None:
    root = make_tree(tmp_path)
    assert run(root, NEW).returncode == 0
    result = run(root, "--check")
    assert result.returncode == 0, result.stderr
    assert "版本标记一致" in result.stdout


def test_check_detects_a_stale_marker(tmp_path: Path) -> None:
    root = make_tree(tmp_path)
    assert run(root, NEW).returncode == 0
    # 人为把一处标记改回旧版本（模拟漏改）
    doc = root / "docs" / "出题质检与难度口径.md"
    doc.write_text(doc.read_text(encoding="utf-8").replace(NEW, OLD), encoding="utf-8", newline="")

    result = run(root, "--check")
    assert result.returncode == 1
    assert "出题质检与难度口径.md" in result.stderr


def test_missing_release_notes_section_writes_nothing(tmp_path: Path) -> None:
    root = make_tree(tmp_path, with_new_section=False)
    result = run(root, NEW)
    assert result.returncode == 1
    assert "更新摘要" in result.stderr
    # 一个文件都不能改：不允许留下半成品
    assert read(root, "VERSION") == f"{OLD}\n"
    assert f'"{OLD}"' in read(root, "Cargo.toml")


def test_dry_run_does_not_write(tmp_path: Path) -> None:
    root = make_tree(tmp_path)
    result = run(root, NEW, "--dry-run")
    assert result.returncode == 0, result.stderr
    assert "将要" in result.stdout
    assert read(root, "VERSION") == f"{OLD}\n"


def test_rejects_bad_version_format(tmp_path: Path) -> None:
    root = make_tree(tmp_path)
    for bad in ("1.3", "v1.3.0", "1.3.0.1", ""):
        result = run(root, bad) if bad else run(root, "")
        assert result.returncode != 0, f"{bad!r} 应被拒绝"
    assert read(root, "VERSION") == f"{OLD}\n"


def test_real_repo_markers_are_consistent() -> None:
    """真实仓库的标记必须一致——这条会在 CI 里跑，发版漏改立刻红。"""
    result = subprocess.run(
        [sys.executable, str(SCRIPT), "--check"],
        capture_output=True,
        encoding="utf-8",
        errors="replace",
        timeout=60,
        cwd=ROOT,
        env={**os.environ, "PYTHONIOENCODING": "utf-8"},
    )
    assert result.returncode == 0, result.stderr + result.stdout


@pytest.mark.parametrize("rel", ["VERSION", "Cargo.toml", "admin/pyproject.toml"])
def test_three_version_sources_agree(rel: str) -> None:
    """三处版本来源必须一致（脚本 --check 的前置条件，单独再守一道）。"""
    version = (ROOT / "VERSION").read_text(encoding="utf-8").strip()
    text = (ROOT / rel).read_text(encoding="utf-8")
    if rel == "VERSION":
        assert text.strip() == version
    else:
        assert f'"{version}"' in text
