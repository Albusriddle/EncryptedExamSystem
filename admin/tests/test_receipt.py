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


def test_public_key_verification_without_private_key(tmp_path: Path) -> None:
    from app.core.receipt import verify_receipt_detailed

    sheet = tmp_path / "a.eea"
    sheet.write_bytes(b"answer-sheet")
    key = Ed25519PrivateKey.generate()
    data = build_receipt([(sheet, {"student_id": 1, "session_id": 2})], signer=key)
    ok, reason = verify_receipt_detailed(data)  # 只用收据里的公钥
    assert ok and reason == "ok"


def test_receipt_reports_wrong_key_and_tamper(tmp_path: Path) -> None:
    from app.core.receipt import verify_receipt_detailed

    sheet = tmp_path / "a.eea"
    sheet.write_bytes(b"answer-sheet")
    key = Ed25519PrivateKey.generate()
    data = build_receipt([(sheet, {"student_id": 1, "session_id": 2})], signer=key)
    ok, reason = verify_receipt_detailed(data, Ed25519PrivateKey.generate())
    assert not ok and reason == "wrong_key"
    data["sheets"][0]["student_id"] = 99
    ok, reason = verify_receipt_detailed(data)
    assert not ok and reason in {"bad_signature", "fingerprint_mismatch"}


def test_receipt_relative_base_dir(tmp_path: Path) -> None:
    from app.core.receipt import verify_files

    src = tmp_path / "src"
    src.mkdir()
    sheet = src / "a.eea"
    sheet.write_bytes(b"answer-sheet")
    key = Ed25519PrivateKey.generate()
    data = build_receipt([(sheet, {"student_id": 1, "session_id": 2})], signer=key)
    moved = tmp_path / "archive"
    moved.mkdir()
    (moved / "a.eea").write_bytes(b"answer-sheet")
    assert verify_files(data, base_dir=moved) == []


def test_receipt_package_hash(tmp_path: Path) -> None:
    from app.core.receipt import verify_package

    pkg = tmp_path / "pkg.exe"
    pkg.write_bytes(b"package")
    sheet = tmp_path / "a.eea"
    sheet.write_bytes(b"sheet")
    key = Ed25519PrivateKey.generate()
    data = build_receipt(
        [(sheet, {"student_id": 1, "session_id": 2})], package=str(pkg), signer=key
    )
    assert verify_package(data, pkg) == ""
    pkg.write_bytes(b"other")
    assert "不一致" in verify_package(data, pkg)
