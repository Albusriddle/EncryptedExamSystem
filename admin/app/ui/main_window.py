"""主窗口：四个 Tab + 工具栏 + 状态栏。

## 分层

`ui/` 只做"收集输入 → 交给 core 校验 → 显示结果"。所有业务规则都在
`core/models.py` 与 `core/state.py`，本目录不重复实现任何校验。

## 数据流

```
用户操作 → Tab 修改 ProjectState →  emits changed → 主窗口刷新状态栏
                                            ↓
                                  打包 Tab 读取同一份 state
```
"""

from __future__ import annotations

from pathlib import Path

from PySide6.QtCore import Qt
from PySide6.QtGui import QAction
from PySide6.QtWidgets import (
    QFileDialog,
    QLabel,
    QMainWindow,
    QMessageBox,
    QSizePolicy,
    QTabWidget,
    QWidget,
)

from app.branding import branding
from app.core.models import Manifest
from app.core.state import ProjectState
from app.ui.tab_banks import BanksTab
from app.ui.tab_build import BuildTab
from app.ui.tab_sessions import SessionsTab
from app.ui.tab_students import StudentsTab
from app.version import app_version, watermark


class MainWindow(QMainWindow):
    def __init__(self) -> None:
        super().__init__()
        self.state = ProjectState()
        self.project_path: Path | None = None

        self.setWindowTitle("保密考试系统 · 管理端")
        self.resize(1080, 760)

        self._build_toolbar()
        self._build_tabs()
        self._build_statusbar()
        self._refresh_status()

    # ---------------------------------------------------------------- 构建

    def _build_toolbar(self) -> None:
        bar = self.addToolBar("主")
        bar.setMovable(False)

        act_new = QAction("新建工程", self)
        act_new.triggered.connect(self.new_project)
        bar.addAction(act_new)

        act_open = QAction("打开工程…", self)
        act_open.triggered.connect(self.open_project)
        bar.addAction(act_open)

        act_save = QAction("保存工程…", self)
        act_save.triggered.connect(self.save_project)
        bar.addAction(act_save)

        bar.addSeparator()

        act_check = QAction("检查配置", self)
        act_check.setToolTip("一次性列出所有会阻止打包的问题")
        act_check.triggered.connect(self.check_config)
        bar.addAction(act_check)

        # 水印：紧接在「检查配置」右侧。
        # 用可伸缩的 spacer 把后面的控件推到最右，窗口放大时水印仍在右边。
        spacer = QWidget()
        spacer.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Preferred)
        bar.addWidget(spacer)
        self.watermark_label = QLabel(watermark())
        self.watermark_label.setObjectName("watermark")
        # 直接用版本号，不要从水印字符串里切：
        # `watermark().split()[1]` 依赖 "ABBR vX.Y.Z Powered by N" 这个形状，
        # 品牌缩写一旦带空格就会 IndexError。
        self.watermark_label.setToolTip(f"{branding().display_name()} · 版本 {app_version()}")
        # 显式颜色：palette(mid) 在深色主题下几乎与背景同色，实测看不见。
        self.watermark_label.setStyleSheet("color: #9aa0a6; padding-right: 12px; font-size: 11px;")
        bar.addWidget(self.watermark_label)

    def _build_tabs(self) -> None:
        self.tabs = QTabWidget()
        self.banks_tab = BanksTab(self.state)
        self.students_tab = StudentsTab(self.state)
        self.sessions_tab = SessionsTab(self.state)
        self.build_tab = BuildTab(self.state)

        # 任何 Tab 改了数据都刷新状态栏与其他 Tab 的联动视图
        for tab in (self.banks_tab, self.students_tab, self.sessions_tab):
            tab.changed.connect(self._on_changed)

        self.tabs.addTab(self.banks_tab, "题库")
        self.tabs.addTab(self.students_tab, "学生")
        self.tabs.addTab(self.sessions_tab, "场次")
        self.tabs.addTab(self.build_tab, "打包")
        # 收卷判分尚未实现（学生端还没有作答与交卷功能）
        placeholder = QWidget()
        tip = QLabel(
            "收卷与判分将在学生端支持作答后开放。\n\n"
            "当前版本学生端为「试卷展示器」：可解锁、可看题，但暂不支持作答与交卷。",
            placeholder,
        )
        tip.setAlignment(Qt.AlignmentFlag.AlignCenter)
        tip.setWordWrap(True)
        lay = placeholder.layout()
        if lay is None:
            from PySide6.QtWidgets import QVBoxLayout

            lay = QVBoxLayout(placeholder)
        lay.addWidget(tip)
        self.tabs.addTab(placeholder, "收卷判分")
        self.tabs.setTabEnabled(4, False)

        self.setCentralWidget(self.tabs)

    def _build_statusbar(self) -> None:
        self.status_label = QLabel()
        self.statusBar().addPermanentWidget(self.status_label)
        self.statusBar().showMessage("就绪")

    # ---------------------------------------------------------------- 刷新

    def _on_changed(self) -> None:
        self._refresh_status()
        # 题库/学生变了，场次 Tab 里的可选项也要跟着变
        self.sessions_tab.refresh_choices()
        self.build_tab.refresh_preview()

    def _refresh_status(self) -> None:
        self.status_label.setText(self.state.summary())
        title = "保密考试系统 · 管理端"
        if self.project_path:
            title += f" — {self.project_path.name}"
        if self.state.validate():
            title += "  ⚠ 配置有问题"
        self.setWindowTitle(title)

    # ---------------------------------------------------------------- 动作

    def new_project(self) -> None:
        if not self._confirm_discard():
            return
        self.state = ProjectState()
        self.project_path = None
        self._reload_tabs()
        self.statusBar().showMessage("已新建工程", 4000)

    def open_project(self) -> None:
        if not self._confirm_discard():
            return
        path, _ = QFileDialog.getOpenFileName(self, "打开工程", "", "工程 (*.json)")
        if not path:
            return
        try:
            self.state = ProjectState.load(Path(path))
        except Exception as e:  # noqa: BLE001 - 展示给用户，不吞掉
            QMessageBox.critical(self, "打开失败", f"{path}\n\n{e}")
            return
        self.project_path = Path(path)
        self._reload_tabs()
        missing = set(self.state.bank_paths) - set(self.state.banks)
        if missing:
            QMessageBox.warning(
                self,
                "题库缺失",
                "以下题库的源文件无法读取，请重新导入：\n\n" + "\n".join(sorted(missing)),
            )
        self.statusBar().showMessage(f"已打开 {path}", 5000)

    def save_project(self) -> None:
        path, _ = QFileDialog.getSaveFileName(
            self,
            "保存工程",
            self.project_path.name if self.project_path else "project.json",
            "工程 (*.json)",
        )
        if not path:
            return
        try:
            self.state.save(Path(path))
        except Exception as e:  # noqa: BLE001
            QMessageBox.critical(self, "保存失败", str(e))
            return
        self.project_path = Path(path)
        self._refresh_status()
        self.statusBar().showMessage(f"已保存 {path}", 5000)

    def check_config(self) -> None:
        problems = self.state.validate()
        if not problems:
            QMessageBox.information(self, "检查通过", "配置完整，可以打包。")
            return
        QMessageBox.warning(
            self,
            "配置存在问题",
            "以下问题会阻止打包：\n\n" + "\n".join(f"· {p}" for p in problems),
        )

    def _confirm_discard(self) -> bool:
        if not (self.state.banks or self.state.sessions):
            return True
        ret = QMessageBox.question(
            self,
            "确认",
            "当前工程未保存的修改会丢失，继续吗？",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        return ret == QMessageBox.StandardButton.Yes

    def _reload_tabs(self) -> None:
        """工程切换后重建全部 Tab。

        直接复用 `_build_tabs()`，而不是手写"删掉前 N 个再插入"——
        旧实现连写四次 `removeTab(0)`，隐含依赖"当前共 5 个 Tab"，
        以后加一个 Tab 就会错位，而且占位页会被漏掉。
        """
        self._build_tabs()
        self._refresh_status()


def make_manifest(state: ProjectState) -> Manifest:
    """供打包 Tab 使用的入口（保留在此以便将来加签名等后处理）。"""
    return state.build_manifest()
