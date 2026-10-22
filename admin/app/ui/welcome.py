"""启动欢迎弹窗：版本、仓库、许可证、致谢、Star 邀请。

## 为什么单独写一个对话框而不是 QMessageBox

需求里有三类可点击链接（仓库 + 三个人主页），QMessageBox 的富文本
虽然支持 <a>，但排版受限制、也没有"不再提示"。这里用自定义 QDialog，
既能把链接做成真正的可点击超链接，也能记住用户的选择。
"""

from __future__ import annotations

import os
from pathlib import Path

from PySide6.QtCore import QSettings, Qt
from PySide6.QtWidgets import (
    QCheckBox,
    QDialog,
    QDialogButtonBox,
    QLabel,
    QVBoxLayout,
)

from app.branding import branding
from app.version import app_version

_SETTINGS_KEY = "welcome/hidden"


def settings_path() -> Path:
    r"""设置文件位置：`%LOCALAPPDATA%\ExamSystem\settings.ini`。

    **不用注册表后端**：实测在教育网/受限账户的机器上，QSettings 默认的
    注册表写入会返回 Status.AccessError，"不再提示"于是永远存不住——
    用户勾了也没用。文件后端不依赖注册表，行为可预期。
    与客户端的 skip_welcome 标记放在同一目录，便于排查。
    """
    base = os.environ.get("LOCALAPPDATA") or os.environ.get("APPDATA") or str(Path.home())
    return Path(base) / "ExamSystem" / "settings.ini"


def _settings() -> QSettings:
    path = settings_path()
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
    except OSError:
        pass
    return QSettings(str(path), QSettings.Format.IniFormat)


def _rich_text() -> str:
    b = branding()
    credits = "<br>".join(f'· {c.role}：<a href="{c.url}">{c.name}</a>' for c in b.credits)
    return f"""
<div style="font-size:13px; line-height:150%;">
  <p style="font-size:16px; margin-bottom:2px;">
    <b>欢迎使用 {b.display_name()}</b>
  </p>
  <p style="color:#888;">版本 {app_version()} · {b.license_name}</p>

  <p>{b.tagline}</p>

  <p>项目地址：<br>
     <a href="{b.repo_url}">{b.repo_url}</a><br>
     许可证：{b.license_name}（{b.license}）
  </p>

  <p><b>{b.credits_title}</b><br>{credits}</p>

  <p style="color:#2a7a2a;"><b>{b.star_hint}</b></p>
</div>
"""


class WelcomeDialog(QDialog):
    def __init__(self, parent=None) -> None:  # noqa: ANN001
        super().__init__(parent)
        self.setWindowTitle("欢迎")
        self.setMinimumWidth(520)

        lay = QVBoxLayout(self)
        label = QLabel(_rich_text())
        label.setTextFormat(Qt.TextFormat.RichText)
        label.setOpenExternalLinks(True)  # 让 <a> 直接调起浏览器
        label.setWordWrap(True)
        lay.addWidget(label)

        self.dont_show = QCheckBox("不再提示")
        lay.addWidget(self.dont_show)

        buttons = QDialogButtonBox(QDialogButtonBox.StandardButton.Ok)
        buttons.button(QDialogButtonBox.StandardButton.Ok).setText("开始使用")
        buttons.accepted.connect(self.accept)
        lay.addWidget(buttons)

    def should_remember(self) -> bool:
        return self.dont_show.isChecked()


def show_welcome_if_needed(parent=None) -> None:  # noqa: ANN001
    """按用户上次选择决定是否弹出。"""
    settings = _settings()
    if settings.value(_SETTINGS_KEY, False, type=bool):
        return
    dlg = WelcomeDialog(parent)
    dlg.exec()
    if dlg.should_remember():
        settings.setValue(_SETTINGS_KEY, True)
        settings.sync()
        if settings.status() != QSettings.Status.NoError:
            # 存不住也不要紧：下次还会弹，但至少不崩
            pass
