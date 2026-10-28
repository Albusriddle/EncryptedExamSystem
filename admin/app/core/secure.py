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

MAGIC_KEY = b"EESKEY02"
MAGIC_PROJECT = b"EESPROJ3"
# v0.10.0 旧格式：magic|salt|nonce|ct，KDF 参数固定
MAGIC_KEY_OLD = b"EESKEY01"
MAGIC_PROJECT_OLD = b"EESPROJ2"
SALT_LEN = 16
NONCE_LEN = 12
HEADER_LEN = 1 + 2 + 4 + 1  # version + time + memory + parallel

# 与 Rust 端一致的强度；教师机通常不慢，3 次迭代约 0.1–0.3s
ARGON2_TIME = 3
ARGON2_MEMORY = 65536
ARGON2_PARALLEL = 1
# 加密文件头来自外部输入，必须在调用 Argon2 前限幅，避免恶意工程/备份造成 OOM。
MAX_KDF_TIME = 16
MAX_KDF_MEMORY = 1_048_576  # KiB = 1 GiB
MAX_KDF_PARALLEL = 8

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


def _derive_with(passphrase: str, salt: bytes, time_cost: int, memory: int, parallel: int) -> bytes:
    if not passphrase:
        raise ValueError("口令不能为空")
    if not (1 <= time_cost <= MAX_KDF_TIME):
        raise ValueError("加密文件头 KDF 时间参数异常")
    if not (8 * 1024 <= memory <= MAX_KDF_MEMORY):
        raise ValueError("加密文件头 KDF 内存参数异常")
    if not (1 <= parallel <= MAX_KDF_PARALLEL):
        raise ValueError("加密文件头 KDF 并行参数异常")
    return hash_secret_raw(
        secret=passphrase.encode("utf-8"),
        salt=salt,
        time_cost=time_cost,
        memory_cost=memory,
        parallelism=parallel,
        hash_len=32,
        type=Type.ID,
    )


def seal(magic: bytes, passphrase: str, plaintext: bytes) -> bytes:
    salt = os.urandom(SALT_LEN)
    nonce = os.urandom(NONCE_LEN)
    key = _derive(passphrase, salt)
    header = (
        magic
        + bytes([1])
        + ARGON2_TIME.to_bytes(2, "big")
        + ARGON2_MEMORY.to_bytes(4, "big")
        + bytes([ARGON2_PARALLEL])
    )
    ct = AESGCM(key).encrypt(nonce, plaintext, header)
    return header + salt + nonce + ct


def _unseal_new(magic: bytes, passphrase: str, blob: bytes) -> bytes:
    if len(blob) < len(magic) + HEADER_LEN + SALT_LEN + NONCE_LEN + 16:
        raise ValueError("文件格式不正确或已损坏")
    off = len(magic)
    version = blob[off]
    if version != 1:
        raise ValueError(f"不支持的加密文件版本：{version}")
    t = int.from_bytes(blob[off + 1 : off + 3], "big")
    m = int.from_bytes(blob[off + 3 : off + 7], "big")
    p = blob[off + 7]
    off += HEADER_LEN
    salt = blob[off : off + SALT_LEN]
    nonce = blob[off + SALT_LEN : off + SALT_LEN + NONCE_LEN]
    ct = blob[off + SALT_LEN + NONCE_LEN :]
    key = _derive_with(passphrase, salt, t, m, p)
    try:
        return AESGCM(key).decrypt(nonce, ct, magic + blob[len(magic) : off])
    except InvalidTag as e:
        raise ValueError("口令错误或文件已损坏") from e


def unseal(magic: bytes, passphrase: str, blob: bytes) -> bytes:
    old = {MAGIC_KEY: MAGIC_KEY_OLD, MAGIC_PROJECT: MAGIC_PROJECT_OLD}.get(magic)
    if blob.startswith(magic):
        return _unseal_new(magic, passphrase, blob)
    if old and blob.startswith(old):
        if len(blob) < len(old) + SALT_LEN + NONCE_LEN + 16:
            raise ValueError("文件格式不正确或已损坏")
        off = len(old)
        salt = blob[off : off + SALT_LEN]
        nonce = blob[off + SALT_LEN : off + SALT_LEN + NONCE_LEN]
        ct = blob[off + SALT_LEN + NONCE_LEN :]
        key = _derive(passphrase, salt)
        try:
            return AESGCM(key).decrypt(nonce, ct, old)
        except InvalidTag as e:
            raise ValueError("口令错误或文件已损坏") from e
    raise ValueError("文件格式不正确或已损坏")


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
    return data.startswith(MAGIC_PROJECT) or data.startswith(MAGIC_PROJECT_OLD)


def encrypt_project(data: bytes, passphrase: str) -> bytes:
    return seal(MAGIC_PROJECT, passphrase, data)


def decrypt_project(data: bytes, passphrase: str) -> bytes:
    return unseal(MAGIC_PROJECT, passphrase, data)
