"""Linux 用户目录必须遵循 XDG，不能写入解释器或安装目录。"""

from pathlib import Path

import pytest

from app.core import paths


@pytest.fixture(autouse=True)
def clean_user_dirs(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    for name in ("LOCALAPPDATA", "APPDATA", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_DATA_HOME"):
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setattr(paths.sys, "platform", "linux")
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: tmp_path))


def test_settings_honor_xdg_config_home(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path / "配置"))
    assert paths.settings_path() == tmp_path / "配置" / "ExamSystem" / "settings.ini"


def test_settings_default_to_dot_config(tmp_path: Path) -> None:
    assert paths.settings_path() == tmp_path / ".config" / "ExamSystem" / "settings.ini"


def test_relative_xdg_config_home_is_ignored(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setenv("XDG_CONFIG_HOME", "relative/config")
    assert paths.settings_path() == tmp_path / ".config" / "ExamSystem" / "settings.ini"


@pytest.mark.parametrize("platform", ["linux", "win32"])
def test_explicit_appdata_override_is_preserved(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, platform: str
) -> None:
    monkeypatch.setattr(paths.sys, "platform", platform)
    monkeypatch.setenv("LOCALAPPDATA", str(tmp_path / "local"))
    assert paths.settings_path() == tmp_path / "local" / "ExamSystem" / "settings.ini"
    assert paths.state_dir() == tmp_path / "local" / "ExamSystem"
    assert paths.data_dir() == tmp_path / "local" / "ExamSystem"


@pytest.mark.parametrize("value", ["", "relative/state"])
def test_state_directory_has_writable_home_fallback(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, value: str
) -> None:
    monkeypatch.setenv("XDG_STATE_HOME", value)
    assert paths.state_dir() == tmp_path / ".local/state" / "ExamSystem"


@pytest.mark.parametrize("value", ["", "relative/data", "absolute"])
def test_demo_data_directory_honors_absolute_xdg_only(tmp_path, monkeypatch, value) -> None:
    raw = str(tmp_path / "数据") if value == "absolute" else value
    monkeypatch.setenv("XDG_DATA_HOME", raw)
    root = Path(raw) if value == "absolute" else tmp_path / ".local/share"
    assert paths.data_dir() == root / "ExamSystem"


@pytest.mark.parametrize("platform, suffix", [("linux", ""), ("win32", ".exe")])
def test_native_executable_names(
    monkeypatch: pytest.MonkeyPatch, platform: str, suffix: str
) -> None:
    monkeypatch.setattr(paths.sys, "platform", platform)
    assert paths.executable_name("StudentClient") == f"StudentClient{suffix}"
    if platform == "linux":
        assert paths.package_file_filter().split(";;")[0].endswith("(*)")
    else:
        assert paths.package_file_filter().split(";;")[0].endswith("(*.exe)")
