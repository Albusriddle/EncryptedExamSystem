"""答题卡收据：签名、验签、文件哈希校验。"""

from __future__ import annotations

from pathlib import Path

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from app.core.receipt import build_receipt, verify_files, verify_receipt


def test_receipt_roundtrip(tmp_path: Path) -> None:
    sheet = tmp_path / "a.eea"
    sheet.write_bytes(b"answer-sheet")
    key = Ed25519PrivateKey.generate()
    data = build_receipt(
        [(sheet, {"student_id": 1, "session_id": 2, "submitted": True})],
        package="pkg.exe",
        signer=key,
    )
    assert verify_receipt(data, key)
    assert verify_files(data) == []


def test_receipt_detects_tamper(tmp_path: Path) -> None:
    sheet = tmp_path / "a.eea"
    sheet.write_bytes(b"answer-sheet")
    key = Ed25519PrivateKey.generate()
    data = build_receipt([(sheet, {"student_id": 1, "session_id": 2})], signer=key)
    sheet.write_bytes(b"tampered")
    assert verify_files(data), "文件被改后必须报哈希不一致"
    assert not verify_receipt(data, Ed25519PrivateKey.generate()), "换钥匙验签必须失败"
