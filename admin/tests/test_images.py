"""题干内嵌图片的校验（Qt-free）。"""

from __future__ import annotations

import base64

from app.core.images import MAX_BYTES, check_bank_images, check_stem_images
from app.core.models import ExamBank, Question


def _png(width: int, height: int, extra: int = 0) -> str:
    """构造一个只含合法头部的假 PNG（校验只读头部，不解码像素）。"""
    raw = (
        b"\x89PNG\r\n\x1a\n"
        + (13).to_bytes(4, "big")
        + b"IHDR"
        + width.to_bytes(4, "big")
        + height.to_bytes(4, "big")
        + b"\x08\x06\x00\x00\x00"
        + b"\x00" * extra
    )
    return "data:image/png;base64," + base64.b64encode(raw).decode()


def test_valid_image_has_no_problem() -> None:
    assert check_stem_images(f"![图]({_png(800, 600)})") == []


def test_oversized_dimensions_are_reported() -> None:
    problems = check_stem_images(f"![大图]({_png(1600, 900)})")
    assert problems and "1600×900" in problems[0]


def test_oversized_bytes_are_reported() -> None:
    problems = check_stem_images(f"![大文件]({_png(100, 100, extra=MAX_BYTES)})")
    assert any("超过建议上限" in p for p in problems)


def test_broken_base64_is_reported() -> None:
    problems = check_stem_images("![坏](data:image/png;base64,AAAA====)")
    assert problems, "损坏的 base64 必须被挑出来"


def test_bank_reports_question_number() -> None:
    bank = ExamBank(
        exam_id="m",
        title="数学",
        pick=1,
        questions=[
            Question(id="q1", type="single", stem="1+1?", options=["1", "2"]),
            Question(
                id="q2", type="single", stem=f"看图 ![图]({_png(2000, 100)})", options=["1", "2"]
            ),
        ],
    )
    problems = check_bank_images(bank)
    assert len(problems) == 1 and "第 2 题" in problems[0]


def test_to_data_uri_roundtrip() -> None:
    from app.core.images import image_size, to_data_uri

    raw = base64.b64decode(_png(640, 480).split("base64,")[1])
    uri = to_data_uri(raw, "image/png")
    assert uri.startswith("data:image/png;base64,")
    assert image_size(raw, "image/png") == (640, 480)
    assert check_stem_images(f"![图]({uri})") == []
