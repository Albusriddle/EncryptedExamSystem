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

import hashlib
from pathlib import Path

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
from app.ui.app_settings import AUTOLOAD, LAST_PROJECT, WARNED_EESPROJ, projects_dir, settings
from app.ui.tab_answers import AnswersTab
from app.ui.tab_banks import BanksTab
from app.ui.tab_build import BuildTab
from app.ui.tab_sessions import SessionsTab
from app.ui.tab_students import StudentsTab
from app.version import app_version, watermark


class MainWindow(QMainWindow):
    def __init__(self, autoload: bool = True) -> None:
        super().__init__()
        self.state = ProjectState()
        self.project_path: Path | None = None
        #: 是否有未保存的改动。没有它的话，只要导入过题库/建过场次，
        #: 每次新建或打开工程都会问"未保存的修改会丢失"——哪怕刚保存过。
        #: 教师被问烦了就会无脑点"是"，真正该拦的时候反而拦不住。
        self._dirty = False

        self.setWindowTitle(self._window_title())
        self.resize(1080, 760)

        self._build_toolbar()
        self._build_tabs()
        self._build_statusbar()
        self._refresh_status()
        if autoload:
            self._autoload_last_project()

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
        self.answers_tab = AnswersTab(self.state)

        # 任何 Tab 改了数据都刷新状态栏与其他 Tab 的联动视图
        for tab in (self.banks_tab, self.students_tab, self.sessions_tab):
            tab.changed.connect(self._on_changed)

        self.tabs.addTab(self.banks_tab, "题库")
        self.tabs.addTab(self.students_tab, "学生")
        self.tabs.addTab(self.sessions_tab, "场次")
        self.tabs.addTab(self.build_tab, "打包")
        self.build_tab.built.connect(self._on_built)
        self.tabs.addTab(self.answers_tab, "收卷判分")

        self.setCentralWidget(self.tabs)

    def _build_statusbar(self) -> None:
        self.status_label = QLabel()
        self.statusBar().addPermanentWidget(self.status_label)
        self.statusBar().showMessage("就绪")

    # ---------------------------------------------------------------- 刷新

    def _on_changed(self) -> None:
        self._dirty = True
        self._refresh_status()
        # 题库/学生变了，场次 Tab 里的可选项也要跟着变
        self.sessions_tab.refresh_choices()
        self.build_tab.refresh_preview()

    def _refresh_status(self) -> None:
        self.status_label.setText(self.state.summary())
        title = self._window_title()
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
        self._dirty = False
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
        self._dirty = False
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
        self._dirty = False
        settings().setValue(LAST_PROJECT, str(path))
        self._refresh_status()
        self.statusBar().showMessage(f"已保存 {path}", 5000)

    # ---------------------------------------------------------------- 工程自动保存/打开

    def _on_built(self, out: str) -> None:
        """打包成功后自动保存工程。

        **绝不写在考试包输出目录**：那是要发给学生的目录，工程里有标准答案。
        优先覆盖教师自己的工程文件；否则存到 `%LOCALAPPDATA%/ExamSystem/projects/`。
        """
        prod = Path(out)
        if self.project_path is not None:
            proj = self.project_path
        else:
            # 包名相同但目录不同的两个包不能共用一个工程文件（会互相覆盖）：
            # 取输出路径的短哈希做后缀。
            digest = hashlib.blake2s(str(prod.resolve()).encode("utf-8"), digest_size=2).hexdigest()
            proj = projects_dir() / f"{prod.stem}-{digest}.eesproj"

        legacy = prod.with_suffix(".eesproj")
        if legacy.exists() and legacy != proj:
            QMessageBox.warning(
                self,
                "发现旧版工程文件",
                f"考试包目录里有一个旧版工程文件：\n{legacy}\n\n"
                "它包含标准答案，**不要随考试包发给学生**。\n建议手动删除。",
            )

        try:
            self.state.save(proj)
        except Exception as e:  # noqa: BLE001 - 展示给用户
            QMessageBox.warning(self, "工程自动保存失败", f"{proj}\n\n{e}")
            return

        st = settings()
        st.setValue(LAST_PROJECT, str(proj))
        st.setValue(AUTOLOAD, True)
        self.project_path = proj
        self._dirty = False
        self._refresh_status()

        # 按**工程路径**记忆：换电脑/换项目还会再提醒一次（防止误分发）
        if st.value(WARNED_EESPROJ, "", type=str) != str(proj):
            st.setValue(WARNED_EESPROJ, str(proj))
            QMessageBox.information(
                self,
                "工程已自动保存",
                f"工程已保存到：\n{proj}\n\n"
                "⚠ 该文件含标准答案与判分材料，**切勿随考试包一起分发**。\n"
                "下次启动会自动打开它。",
            )
        else:
            self.statusBar().showMessage(f"工程已自动保存：{proj}", 8000)

    def _autoload_last_project(self) -> None:
        """启动时自动打开上次的工程（可在设置里关掉）。"""
        st = settings()
        if not st.value(AUTOLOAD, True, type=bool):
            return
        raw = st.value(LAST_PROJECT, "", type=str)
        if not raw:
            return
        path = Path(raw)
        if not path.is_file():
            self.statusBar().showMessage("上次的工程文件已不存在，已新建空白工程", 8000)
            return
        try:
            self.state = ProjectState.load(path)
        except Exception as e:  # noqa: BLE001 - 自动打开失败不能拦住启动
            self.statusBar().showMessage(f"上次的工程无法打开（{e}），已新建空白工程", 10000)
            return
        self.project_path = path
        self._dirty = False
        self._reload_tabs()
        missing = set(self.state.bank_paths) - set(self.state.banks)
        if missing:
            QMessageBox.warning(
                self,
                "题库缺失",
                "以下题库的源文件无法读取，请重新导入：\n\n" + "\n".join(sorted(missing)),
            )
        self.statusBar().showMessage(f"已打开上次工程：{path}", 8000)

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

    def _window_title(self) -> str:
        """窗口标题统一由 branding 生成——改名时不需要满仓库找字符串。"""
        return f"{branding().product} · 管理端"

    def _confirm_discard(self) -> bool:
        if not self._dirty:
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
