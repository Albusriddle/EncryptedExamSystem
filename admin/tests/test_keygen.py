"""主密钥生成：解析与后台线程（不阻塞 GUI）。"""

from __future__ import annotations

import pytest

pytest.importorskip("PySide6")


@pytest.fixture(scope="module")
def qapp():
    import sys

    from PySide6.QtWidgets import QApplication

    app = QApplication.instance() or QApplication(sys.argv)
    yield app


def test_parse_keygen_output_accepts_valid_and_rejects_garbage() -> None:
    from app.ui.tab_build import parse_keygen_output

    key = "ab" * 32
    assert parse_keygen_output(key + "\n") == key
    with pytest.raises(ValueError):
        parse_keygen_output("not-a-key")


def test_keygen_worker_emits_key(qapp, monkeypatch) -> None:
    from app.ui import tab_build

    class CP:
        returncode = 0
        stdout = "cd" * 32
        stderr = ""

    monkeypatch.setattr(tab_build.subprocess, "run", lambda *a, **k: CP())
    worker = tab_build.KeygenWorker()
    got: list[str] = []
    worker.done.connect(got.append)
    worker.run()
    qapp.processEvents()
    assert got == ["cd" * 32]


def test_keygen_worker_reports_failure(qapp, monkeypatch) -> None:
    from app.ui import tab_build

    class CP:
        returncode = 2
        stdout = ""
        stderr = "boom"

    monkeypatch.setattr(tab_build.subprocess, "run", lambda *a, **k: CP())
    worker = tab_build.KeygenWorker()
    errors: list[str] = []
    worker.failed.connect(errors.append)
    worker.run()
    qapp.processEvents()
    assert errors and "boom" in errors[0]
