"""口令加密的小工具：主密钥备份（.eekey）与加密工程（.eesproj v2）。

格式统一为：magic(8) | salt(16) | nonce(12) | AES-256-GCM 密文。
密钥由 Argon2id(passphrase, salt) 派生；AAD 用 magic，防止格式混用。
"""

from __future__ import annotations

import itertools
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
MAX_KDF_MEMORY = 262_144  # KiB = 256 MiB
MAX_KDF_PARALLEL = 8

_HEX64 = re.compile(r"^[0-9a-fA-F]{64}$")


#: 临时文件名序号：同一目标的并发写（例如后台签发收据与「导出私钥」撞同一路径）
#: 必须各写各的临时文件，否则先 replace 的那个会把对方半截内容顶上目标位置。
_TMP_SEQ = itertools.count(1)


def atomic_write(path: Path, data: bytes, mode: int | None = None) -> None:
    """原子写：先写唯一临时文件，flush + fsync 后再 `os.replace`。

    为什么不能直接 `path.write_bytes(data)`：`.eekey` 是主密钥的**唯一备份**、
    `receipt-key.pem` 是收据签名私钥。直接覆盖写时断电/崩溃会留下半截文件，
    而错误要到「恢复主密钥 / 签发收据」时才暴露——那时已经没有可恢复的源。
    先写临时文件再替换，最坏结果是「还是旧文件」，而不是「文件坏了」。

    `mode` 非空时用 `os.open` 按该权限**创建**临时文件（私钥用 0600）：
    先按 umask 创建再 chmod 会留下一个几毫秒的「别人可读」窗口。

    失败时清理临时文件并原样抛出：调用方（管理端界面）要把异常显示给老师。
    """
    tmp = path.with_name(f"{path.name}.{os.getpid()}.{next(_TMP_SEQ)}.tmp")
    try:
        if mode is None:
            with open(tmp, "wb") as f:
                f.write(data)
                f.flush()
                os.fsync(f.fileno())
        else:
            fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, mode)
            with os.fdopen(fd, "wb") as f:
                f.write(data)
                f.flush()
                os.fsync(f.fileno())
        os.replace(tmp, path)
    except BaseException:
        tmp.unlink(missing_ok=True)
        raise


def restrict_permissions(path: Path) -> str | None:
    """把文件权限收到「仅当前用户」（0600）；成功返回 None，失败返回警告文案。

    为什么返回警告而不是静默 `except OSError: pass`：Windows 上 `chmod` 只切
    「只读」位、并不会设置 ACL（文件本来就在用户私有的 `%LOCALAPPDATA%` 下，
    失败不致命），但**静默吞掉**会让「权限没收紧」这件事永远没人知道；
    收据私钥、PIN 表这类敏感文件必须让调用方有机会提示老师。
    """
    try:
        path.chmod(0o600)
    except OSError as e:
        return f"无法把 {path} 的权限收紧到 0600：{e}"
    return None


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


def backup_master_key(master_hex: str, path: Path, passphrase: str) -> str | None:
    """把主密钥加密备份到 `.eekey`；返回权限收紧的告警（None = 一切正常）。

    为什么把告警**返回**给调用方：`.eekey` 是主密钥的唯一备份，Windows 上
    `chmod` 拿不到 ACL（文件在用户私有的 `%LOCALAPPDATA%` 下，风险低），
    但静默吞掉就等于「权限没收紧」永远没人知道；界面应把告警显示给老师。
    """
    if not _HEX64.match(master_hex.strip()):
        raise ValueError("主密钥必须是 64 位十六进制")
    # 原子写：备份失败时要保证「旧的备份还在」，而不是两份都没了。
    atomic_write(path, seal(MAGIC_KEY, passphrase, master_hex.strip().lower().encode()))
    return restrict_permissions(path)


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
