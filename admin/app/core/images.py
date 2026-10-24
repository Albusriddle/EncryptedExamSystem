"""题干内嵌图片的校验（Qt-free）。

教师把图片以 `data:image/png;base64,…` 嵌进题干；打包前在这里查一遍，
避免"考场里才发现图挂了"。
"""

from __future__ import annotations

import base64
import re

from app.core.models import ExamBank

#: 建议上限：超过就提示教师压缩（学生端硬上限是 4MB/4096px，见 images.rs）
MAX_BYTES = 1_500_000
MAX_SIDE = 1200

_IMG = re.compile(
    r"!\[(?P<alt>[^\]]*)\]\(data:image/(?P<mime>png|jpe?g);base64,(?P<data>[A-Za-z0-9+/=\s]+)\)"
)


def _png_size(raw: bytes) -> tuple[int, int] | None:
    if len(raw) < 24 or not raw.startswith(b"\x89PNG\r\n\x1a\n") or raw[12:16] != b"IHDR":
        return None
    w = int.from_bytes(raw[16:20], "big")
    h = int.from_bytes(raw[20:24], "big")
    return w, h


def _jpeg_size(raw: bytes) -> tuple[int, int] | None:
    if len(raw) < 4 or raw[0:2] != b"\xff\xd8":
        return None
    i = 2
    while i + 9 < len(raw):
        if raw[i] != 0xFF:
            i += 1
            continue
        marker = raw[i + 1]
        if marker in (0xC0, 0xC1, 0xC2, 0xC3):  # SOF0-3
            h = int.from_bytes(raw[i + 5 : i + 7], "big")
            w = int.from_bytes(raw[i + 7 : i + 9], "big")
            return w, h
        seg_len = int.from_bytes(raw[i + 2 : i + 4], "big")
        i += 2 + seg_len
    return None


def image_size(raw: bytes, mime: str) -> tuple[int, int] | None:
    """读取图片宽高（只读头部，不解码像素）。"""
    return _png_size(raw) if mime == "image/png" else _jpeg_size(raw)


def to_data_uri(raw: bytes, mime: str) -> str:
    """把图片字节转成 `data:image/...;base64,...`。"""
    return f"data:{mime};base64," + base64.b64encode(raw).decode("ascii")


def check_stem_images(stem: str) -> list[str]:
    """返回该题干里图片的问题列表（空列表表示没问题）。"""
    problems: list[str] = []
    for m in _IMG.finditer(stem or ""):
        alt = (m.group("alt") or "").strip() or "（无说明）"
        data = re.sub(r"\s+", "", m.group("data"))
        try:
            raw = base64.b64decode(data, validate=True)
        except Exception:  # noqa: BLE001 - 只报告，不中断导入
            problems.append(f"图片「{alt}」的 base64 已损坏，学生端会显示不出来")
            continue
        if len(raw) > MAX_BYTES:
            problems.append(
                f"图片「{alt}」{len(raw) / 1024:.0f} KB，"
                f"超过建议上限 {MAX_BYTES // 1000} KB，请压缩后再打包"
            )
        mime = m.group("mime")
        size = _png_size(raw) if mime == "png" else _jpeg_size(raw)
        if size is None:
            problems.append(f"图片「{alt}」不是有效的 PNG/JPEG，学生端会显示不出来")
        elif size[0] > MAX_SIDE or size[1] > MAX_SIDE:
            problems.append(
                f"图片「{alt}」尺寸 {size[0]}×{size[1]}，超过建议上限 {MAX_SIDE}px，请缩小后再打包"
            )
    return problems


def check_bank_images(bank: ExamBank) -> list[str]:
    """整个题库的图片问题，带题号。"""
    out: list[str] = []
    for i, q in enumerate(bank.questions, start=1):
        for p in check_stem_images(q.stem):
            out.append(f"第 {i} 题：{p}")
    return out
