"""答题卡收据：教师用 Ed25519 对"收卷时刻的文件哈希"签名。

它证明的是"收卷之后这些文件没被改过"，**不证明**学生交卷前没改过自己的卡。
私钥保存在教师机的 appdata（receipt-key.pem），请自行备份。
"""

from __future__ import annotations

import base64
import hashlib
import json
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from app.ui.app_settings import settings_path


def key_path() -> Path:
    return settings_path().parent / "receipt-key.pem"


def load_key(path: Path | None = None) -> Ed25519PrivateKey:
    p = path or key_path()
    if not p.is_file():
        raise FileNotFoundError(f"收据私钥不存在：{p}")
    return serialization.load_pem_private_key(p.read_bytes(), password=None)  # type: ignore[return-value]


def load_or_create_key(path: Path | None = None) -> Ed25519PrivateKey:
    p = path or key_path()
    if p.is_file():
        return load_key(p)
    key = Ed25519PrivateKey.generate()
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_bytes(
        key.private_bytes(
            encoding=serialization.Encoding.PEM,
            format=serialization.PrivateFormat.PKCS8,
            encryption_algorithm=serialization.NoEncryption(),
        )
    )
    try:
        p.chmod(0o600)
    except OSError:
        pass
    return key


def fingerprint(key: Ed25519PrivateKey | None = None) -> str:
    k = key or load_key()
    pub = k.public_key().public_bytes(
        encoding=serialization.Encoding.Raw,
        format=serialization.PublicFormat.Raw,
    )
    return hashlib.sha256(pub).hexdigest()[:16]


def _canonical(data: dict[str, Any]) -> bytes:
    body = {k: v for k, v in data.items() if k != "signature"}
    return json.dumps(body, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()


def build_receipt(
    files: list[tuple[Path, dict[str, Any]]],
    *,
    package: str = "",
    key: Ed25519PrivateKey | None = None,
) -> dict[str, Any]:
    k = key or load_or_create_key()
    sheets = []
    for path, sheet in files:
        sheets.append(
            {
                "file": str(path),
                "sha256": hashlib.sha256(Path(path).read_bytes()).hexdigest(),
                "student_id": int(sheet.get("student_id") or 0),
                "session_id": int(sheet.get("session_id") or 0),
                "submitted": bool(sheet.get("submitted")),
            }
        )
    data: dict[str, Any] = {
        "version": 1,
        "created": datetime.now(UTC).isoformat(timespec="seconds"),
        "package": package,
        "key_fingerprint": fingerprint(k),
        "sheets": sheets,
    }
    data["signature"] = base64.b64encode(k.sign(_canonical(data))).decode("ascii")
    return data


def verify_receipt(data: dict[str, Any], key: Ed25519PrivateKey | None = None) -> bool:
    sig = data.get("signature")
    if not sig:
        return False
    try:
        k = key or load_key()
        k.public_key().verify(base64.b64decode(sig), _canonical(data))
        return True
    except Exception:  # noqa: BLE001 - 任何验签失败都返回 False
        return False


def verify_files(data: dict[str, Any]) -> list[str]:
    """返回问题列表；空列表表示所有文件哈希仍匹配。"""
    issues: list[str] = []
    for s in data.get("sheets", []):
        p = Path(str(s.get("file", "")))
        if not p.is_file():
            issues.append(f"文件不存在：{p}")
            continue
        got = hashlib.sha256(p.read_bytes()).hexdigest()
        if got != s.get("sha256"):
            issues.append(f"哈希不一致（文件被改过）：{p.name}")
    return issues
