"""工程审计日志：追加写 + 哈希链，改一行就会断链。"""

from __future__ import annotations

import hashlib
import json
from datetime import UTC, datetime
from pathlib import Path


class AuditLog:
    def __init__(self, directory: Path) -> None:
        self.directory = Path(directory)
        self.directory.mkdir(parents=True, exist_ok=True)
        stamp = datetime.now().strftime("%Y%m")
        self.path = self.directory / f"audit-{stamp}.jsonl"
        self._last = self._tail_hash()

    def _tail_hash(self) -> str:
        if not self.path.is_file():
            return ""
        last = ""
        try:
            with self.path.open("r", encoding="utf-8") as f:
                for line in f:
                    line = line.strip()
                    if line:
                        last = json.loads(line).get("hash", "")
        except (OSError, json.JSONDecodeError):
            return ""
        return last

    def append(self, event: str, **fields) -> str:
        entry = {
            "ts": datetime.now(UTC).isoformat(timespec="seconds"),
            "event": event,
            "prev": self._last,
            **fields,
        }
        raw = json.dumps(entry, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
        digest = hashlib.blake2s(raw.encode("utf-8"), digest_size=16).hexdigest()
        entry["hash"] = digest
        with self.path.open("a", encoding="utf-8", newline="\n") as f:
            f.write(json.dumps(entry, ensure_ascii=False, sort_keys=True) + "\n")
        self._last = digest
        return digest


def verify_file(path: Path) -> bool:
    """校验哈希链；返回 False 表示被改过/损坏。"""
    prev = ""
    try:
        lines = Path(path).read_text(encoding="utf-8").splitlines()
    except OSError:
        return False
    for line in lines:
        if not line.strip():
            continue
        try:
            entry = json.loads(line)
        except json.JSONDecodeError:
            return False
        digest = entry.pop("hash", None)
        if entry.get("prev") != prev or not digest:
            return False
        raw = json.dumps(entry, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
        if hashlib.blake2s(raw.encode("utf-8"), digest_size=16).hexdigest() != digest:
            return False
        prev = digest
    return True
