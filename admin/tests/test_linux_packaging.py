"""发布脚本必须打包原生 Linux 产物，并保留模板及执行权限。"""

from __future__ import annotations

import json
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]


@pytest.mark.skipif(sys.platform != "linux", reason="原生 Linux 发布包")
def test_linux_release_zip_contains_native_tools(tmp_path: Path) -> None:
    root = tmp_path / "repo"

    def put(name: str, data: bytes = b"fixture") -> Path:
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return path

    script = put("scripts/make-release-zip.sh")
    shutil.copy2(ROOT / "scripts" / script.name, script)
    for name in ("ExamAdmin", "_internal/exam-builder", "_internal/exam-server", "stub-template"):
        put(f"dist/ExamAdmin/{name}").chmod(0o755)
    # 已随包复制的模板不能被旧 target 产物覆盖。
    put("target/release/exam-client", b"outdated template")
    for name in ("教师使用手册.md", "风险接受记录.md"):
        put(f"docs/{name}", "# 测试文档\n".encode())
    put("tools/bank_qc/README.md", b"# Test tools\n")
    for name in ("math", "english"):
        put(f"banks/demo/{name}.json", json.dumps({"questions": []}).encode())

    result = subprocess.run(
        ["bash", str(script), "test"],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=60,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    archive = root / "dist" / "ExamAdmin-test-linux.zip"
    assert archive.is_file()
    with zipfile.ZipFile(archive) as bundle:
        base = "ExamAdmin-test/ExamAdmin/"
        for name in ("ExamAdmin", "exam-builder", "exam-server", "stub-template"):
            item = bundle.getinfo(base + name)
            assert (item.external_attr >> 16) & 0o111, f"{name} 缺少执行权限"
        assert bundle.read(base + "stub-template") == b"fixture"
        assert not any(name.endswith(".exe") for name in bundle.namelist())
