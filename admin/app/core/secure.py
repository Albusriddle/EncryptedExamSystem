"""口令加密的小工具：主密钥备份（.eekey）与加密工程（.eesproj v2）。

格式统一为：magic(8) | salt(16) | nonce(12) | AES-256-GCM 密文。
密钥由 Argon2id(passphrase, salt) 派生；AAD 用 magic，防止格式混用。
"""

from __future__ import annotations

import os
import re
from pathlib import Path

from argon2.low_level import Type, hash_secret_raw
from cryptography.exceptions import InvalidTag
from cryptography.hazmat.primitives.ciphers.aead import AESGCM

MAGIC_KEY = b"EESKEY01"
MAGIC_PROJECT = b"EESPROJ2"
SALT_LEN = 16
NONCE_LEN = 12

# 与 Rust 端一致的强度；教师机通常不慢，3 次迭代约 0.1–0.3s
ARGON2_TIME = 3
ARGON2_MEMORY = 65536
ARGON2_PARALLEL = 1

_HEX64 = re.compile(r"^[0-9a-fA-F]{64}$")


def _derive(passphrase: str, salt: bytes) -> bytes:
    if not passphrase:
        raise ValueError("口令不能为空")
    return hash_secret_raw(
        secret=passphrase.encode("utf-8"),
        salt=salt,
        time_cost=ARGON2_TIME,
        memory_cost=ARGON2_MEMORY,
        parallelism=ARGON2_PARALLEL,
        hash_len=32,
        type=Type.ID,
    )


def seal(magic: bytes, passphrase: str, plaintext: bytes) -> bytes:
    salt = os.urandom(SALT_LEN)
    nonce = os.urandom(NONCE_LEN)
    key = _derive(passphrase, salt)
    ct = AESGCM(key).encrypt(nonce, plaintext, magic)
    return magic + salt + nonce + ct


def unseal(magic: bytes, passphrase: str, blob: bytes) -> bytes:
    if not blob.startswith(magic) or len(blob) < len(magic) + SALT_LEN + NONCE_LEN + 16:
        raise ValueError("文件格式不正确或已损坏")
    off = len(magic)
    salt = blob[off : off + SALT_LEN]
    nonce = blob[off + SALT_LEN : off + SALT_LEN + NONCE_LEN]
    ct = blob[off + SALT_LEN + NONCE_LEN :]
    key = _derive(passphrase, salt)
    try:
        return AESGCM(key).decrypt(nonce, ct, magic)
    except InvalidTag as e:
        raise ValueError("口令错误或文件已损坏") from e


def backup_master_key(master_hex: str, path: Path, passphrase: str) -> None:
    if not _HEX64.match(master_hex.strip()):
        raise ValueError("主密钥必须是 64 位十六进制")
    path.write_bytes(seal(MAGIC_KEY, passphrase, master_hex.strip().lower().encode()))


def restore_master_key(path: Path, passphrase: str) -> str:
    text = unseal(MAGIC_KEY, passphrase, path.read_bytes()).decode("utf-8").strip()
    if not _HEX64.match(text):
        raise ValueError("备份内容不是 64 位十六进制主密钥")
    return text


def is_encrypted_project(data: bytes) -> bool:
    return data.startswith(MAGIC_PROJECT)


def encrypt_project(data: bytes, passphrase: str) -> bytes:
    return seal(MAGIC_PROJECT, passphrase, data)


def decrypt_project(data: bytes, passphrase: str) -> bytes:
    return unseal(MAGIC_PROJECT, passphrase, data)
