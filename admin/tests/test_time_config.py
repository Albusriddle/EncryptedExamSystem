"""签名时间令牌配置（阶段 4.2）：模型校验与工程持久化。"""

from __future__ import annotations

import base64

import pytest
from pydantic import ValidationError

from app.core.models import TimeConfig
from app.core.state import ProjectState


def test_off_allows_empty() -> None:
    assert TimeConfig().mode == "off"


def test_require_needs_https_and_32_byte_key() -> None:
    with pytest.raises(ValidationError):
        TimeConfig(mode="require", url="http://x", pubkey=base64.b64encode(b"x" * 32).decode())
    with pytest.raises(ValidationError):
        TimeConfig(mode="require", url="https://x", pubkey=base64.b64encode(b"x" * 31).decode())
    cfg = TimeConfig(
        mode="require",
        url="https://x/time",
        pubkey=base64.b64encode(b"x" * 32).decode(),
    )
    assert cfg.mode == "require"


def test_state_roundtrip_time(tmp_path) -> None:
    st = ProjectState()
    st.time_mode = "prefer"
    st.time_url = "https://time.example.com/time"
    st.time_pubkey = base64.b64encode(b"k" * 32).decode()
    st.time_max_skew = 120
    p = tmp_path / "a.eesproj"
    st.save(p)
    loaded = ProjectState.load(p)
    assert loaded.time_mode == "prefer"
    assert loaded.time_url == st.time_url
    assert loaded.time_pubkey == st.time_pubkey
    assert loaded.time_max_skew == 120
