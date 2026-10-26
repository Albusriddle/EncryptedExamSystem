"""答题卡收据：教师用 Ed25519 对"收卷时刻的文件哈希"签名。

它证明的是"收卷之后这些文件没被改过"，**不证明**学生交卷前没改过自己的卡。
收据里带公钥，任何拿到收据的人都能验签；私钥在教师机 appdata，请备份。
"""

from __future__ import annotations

import base64
import hashlib
import json
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey, Ed25519PublicKey

from app.core.paths import settings_path


def key_path() -> Path:
    return settings_path().parent / "receipt-key.pem"


def load_key(path: Path | None = None, password: str | None = None) -> Ed25519PrivateKey:
    p = path or key_path()
    if not p.is_file():
        raise FileNotFoundError(f"收据私钥不存在：{p}")
    return serialization.load_pem_private_key(  # type: ignore[return-value]
        p.read_bytes(), password=password.encode() if password else None
    )


def load_or_create_key(path: Path | None = None) -> Ed25519PrivateKey:
    p = path or key_path()
    if p.is_file():
        return load_key(p)
    k = Ed25519PrivateKey.generate()
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_bytes(
        k.private_bytes(
            encoding=serialization.Encoding.PEM,
            format=serialization.PrivateFormat.PKCS8,
            encryption_algorithm=serialization.NoEncryption(),
        )
    )
    try:
        p.chmod(0o600)
    except OSError:
        pass
    return k


def export_key(
    out: Path,
    signer: Ed25519PrivateKey | None = None,
    passphrase: str | None = None,
) -> Path:
    k = signer or load_or_create_key()
    enc = (
        serialization.BestAvailableEncryption(passphrase.encode())
        if passphrase
        else serialization.NoEncryption()
    )
    out.write_bytes(
        k.private_bytes(
            encoding=serialization.Encoding.PEM,
            format=serialization.PrivateFormat.PKCS8,
            encryption_algorithm=enc,
        )
    )
    return out


def public_b64(signer: Ed25519PrivateKey | None = None) -> str:
    k = signer or load_key()
    raw = k.public_key().public_bytes(
        encoding=serialization.Encoding.Raw,
        format=serialization.PublicFormat.Raw,
    )
    return base64.b64encode(raw).decode("ascii")


def fingerprint(signer: Ed25519PrivateKey | None = None, public_key: str | None = None) -> str:
    if public_key:
        raw = base64.b64decode(public_key)
    else:
        k = signer or load_key()
        raw = k.public_key().public_bytes(
            encoding=serialization.Encoding.Raw,
            format=serialization.PublicFormat.Raw,
        )
    return hashlib.sha256(raw).hexdigest()[:16]


def _canonical(data: dict[str, Any]) -> bytes:
    body = {k: v for k, v in data.items() if k != "signature"}
    return json.dumps(body, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()


def build_receipt(
    files: list[tuple[Path, dict[str, Any]]],
    *,
    package: str = "",
    signer: Ed25519PrivateKey | None = None,
) -> dict[str, Any]:
    k = signer or load_or_create_key()
    sheets = []
    for path, sheet in files:
        if Path(path).suffix.lower() != ".eea":
            raise ValueError(f"不是答题卡文件：{path}")
        sheets.append(
            {
                "file": str(path),
                "name": Path(path).name,
                "sha256": hashlib.sha256(Path(path).read_bytes()).hexdigest(),
                "student_id": int(sheet.get("student_id") or 0),
                "session_id": int(sheet.get("session_id") or 0),
                "submitted": bool(sheet.get("submitted")),
            }
        )
    pkg = Path(package) if package else None
    data: dict[str, Any] = {
        "version": 1,
        "created": datetime.now(UTC).isoformat(timespec="seconds"),
        "package": str(package or ""),
        "package_sha256": (
            hashlib.sha256(pkg.read_bytes()).hexdigest() if pkg and pkg.is_file() else ""
        ),
        "public_key": public_b64(k),
        "key_fingerprint": fingerprint(signer=k),
        "sheets": sheets,
    }
    data["signature"] = base64.b64encode(k.sign(_canonical(data))).decode("ascii")
    return data


def verify_receipt_detailed(
    data: dict[str, Any],
    signer: Ed25519PrivateKey | None = None,
    expected_fingerprint: str | None = None,
) -> tuple[bool, str]:
    """返回 (是否通过, 原因)。原因：malformed / missing_public_key / fingerprint_mismatch /
    wrong_key / bad_signature / ok。"""
    if not isinstance(data, dict) or not data.get("signature") or not data.get("sheets"):
        return False, "malformed"
    pub = data.get("public_key")
    if pub:
        try:
            fp = fingerprint(public_key=pub)
            if data.get("key_fingerprint") and fp != data["key_fingerprint"]:
                return False, "fingerprint_mismatch"
        except Exception:  # noqa: BLE001 - 公钥损坏
            return False, "malformed"
    elif signer is not None:
        pub = public_b64(signer)
    else:
        return False, "missing_public_key"
    if expected_fingerprint and data.get("key_fingerprint") != expected_fingerprint:
        return False, "untrusted_key"
    if signer is not None and data.get("key_fingerprint"):
        if fingerprint(signer=signer) != data["key_fingerprint"]:
            return False, "wrong_key"
    try:
        Ed25519PublicKey.from_public_bytes(base64.b64decode(pub)).verify(
            base64.b64decode(data["signature"]), _canonical(data)
        )
    except (InvalidSignature, ValueError):
        return False, "bad_signature"
    return True, "ok"


def verify_receipt(data: dict[str, Any], signer: Ed25519PrivateKey | None = None) -> bool:
    return verify_receipt_detailed(data, signer)[0]


def verify_files(
    data: dict[str, Any], base_dir: Path | None = None, strict: bool = False
) -> list[str]:
    """返回问题列表；base_dir 用于回答"文件被移动"的情况。"""
    issues: list[str] = []
    expected: set[str] = set()
    for s in data.get("sheets", []):
        expected.add(str(s.get("name") or Path(str(s.get("file", ""))).name))
        p = Path(str(s.get("file", "")))
        if not p.is_file() and base_dir is not None:
            p = Path(base_dir) / str(s.get("name") or p.name)
        if not p.is_file():
            issues.append(f"文件不存在：{p}")
            continue
        got = hashlib.sha256(p.read_bytes()).hexdigest()
        if got != s.get("sha256"):
            issues.append(f"哈希不一致（文件被改过）：{p.name}")
    if strict and base_dir is not None:
        for extra in sorted(Path(base_dir).glob("*.eea")):
            if extra.name not in expected:
                issues.append(f"未签名文件：{extra.name}")
    return issues


def verify_package(data: dict[str, Any], package: Path) -> str:
    """返回空串表示包哈希一致；否则返回问题描述。"""
    want = data.get("package_sha256")
    if not want:
        return ""
    if not package.is_file():
        return "考试包不存在，无法核对"
    got = hashlib.sha256(package.read_bytes()).hexdigest()
    return "" if got == want else "考试包哈希不一致（包被替换过）"
