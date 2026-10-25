"""「插入图片…」按钮的行为（不是只测按钮存在）。"""

from __future__ import annotations

from pathlib import Path

import pytest

pytest.importorskip("PySide6")

import sys  # noqa: E402

from PySide6.QtWidgets import QFileDialog, QInputDialog, QMessageBox  # noqa: E402

from app.core.state import ProjectState  # noqa: E402
from app.ui.tab_banks import BanksTab  # noqa: E402


@pytest.fixture(scope="module")
def qapp():
    from PySide6.QtWidgets import QApplication

    app = QApplication.instance() or QApplication(sys.argv)
    yield app


ROOT = Path(__file__).resolve().parents[2]
WORK = ROOT / "target" / "test-insert-image"


def _png(width: int, height: int) -> bytes:
    return (
        b"\x89PNG\r\n\x1a\n"
        + (13).to_bytes(4, "big")
        + b"IHDR"
        + width.to_bytes(4, "big")
        + height.to_bytes(4, "big")
        + b"\x08\x06\x00\x00\x00"
        + b"\x00" * 9
    )


@pytest.fixture()
def tab(qapp, monkeypatch):
    WORK.mkdir(parents=True, exist_ok=True)
    t = BanksTab(ProjectState())
    shown: list[str] = []
    clipboard: list[str] = []

    class FakeClip:
        def setText(self, text: str) -> None:
            clipboard.append(text)

    monkeypatch.setattr(
        "app.ui.tab_banks.QGuiApplication.clipboard", staticmethod(lambda: FakeClip())
    )
    monkeypatch.setattr(
        QMessageBox, "information", staticmethod(lambda *a, **k: shown.append(a[1]))
    )
    monkeypatch.setattr(QMessageBox, "warning", staticmethod(lambda *a, **k: shown.append(a[1])))
    monkeypatch.setattr(QMessageBox, "critical", staticmethod(lambda *a, **k: shown.append(a[1])))
    yield t, clipboard, shown
    t.deleteLater()


def test_inserts_valid_png_and_escapes_alt(tab, monkeypatch) -> None:
    t, clipboard, _ = tab
    img = WORK / "a.png"
    img.write_bytes(_png(640, 480))
    monkeypatch.setattr(
        QFileDialog, "getOpenFileName", staticmethod(lambda *a, **k: (str(img), ""))
    )
    monkeypatch.setattr(QInputDialog, "getText", staticmethod(lambda *a, **k: ("图(左)", True)))

    t.insert_image()

    assert clipboard, "应写入剪贴板"
    snippet = clipboard[0]
    assert snippet.startswith("![图(左）](data:image/png;base64,"), snippet[:40]
    assert snippet.count("](") == 1, "说明里的 ] 必须被替换"


def test_rejects_oversized_dimensions(tab, monkeypatch) -> None:
    t, clipboard, shown = tab
    img = WORK / "big.png"
    img.write_bytes(_png(2000, 100))
    monkeypatch.setattr(
        QFileDialog, "getOpenFileName", staticmethod(lambda *a, **k: (str(img), ""))
    )

    t.insert_image()

    assert not clipboard, "超限图片不应生成片段"
    assert shown and "尺寸过大" in shown[0]


def test_rejects_non_image(tab, monkeypatch) -> None:
    t, clipboard, shown = tab
    img = WORK / "x.png"
    img.write_bytes(b"not a png")
    monkeypatch.setattr(
        QFileDialog, "getOpenFileName", staticmethod(lambda *a, **k: (str(img), ""))
    )

    t.insert_image()

    assert not clipboard
    assert shown and "无法识别" in shown[0]
