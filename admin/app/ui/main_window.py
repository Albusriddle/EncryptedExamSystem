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
import json
from pathlib import Path

from PySide6.QtCore import QTimer, QUrl
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
from app.ui.app_settings import (
    LAST_PROJECT,
    PROJECT_DIR,
    active_projects_dir,
    project_dir_display,
    settings,
)
from app.ui.tab_answers import AnswersTab
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
        #: 是否有未保存的改动。没有它的话，只要导入过题库/建过场次，
        #: 每次新建或打开工程都会问"未保存的修改会丢失"——哪怕刚保存过。
        #: 教师被问烦了就会无脑点"是"，真正该拦的时候反而拦不住。
        self._dirty = False
        self._config_bad = False

        self.setWindowTitle(self._window_title())
        self.resize(1080, 760)

        self._build_toolbar()
        self._build_tabs()
        self._build_statusbar()
        self._refresh_status()
        self.statusBar().showMessage("已新建空白工程" + self._last_project_hint(), 10000)

    # ---------------------------------------------------------------- 构建

    def _build_toolbar(self) -> None:
        bar = self.addToolBar("主")
        bar.setMovable(False)

        act_new = QAction("新建工程", self)
        act_new.triggered.connect(self.new_project)
        bar.addAction(act_new)

        self.act_last = QAction("打开上次工程", self)
        self.act_last.triggered.connect(self.open_last_project)
        bar.addAction(self.act_last)

        act_open = QAction("打开工程…", self)
        act_open.triggered.connect(self.open_project)
        bar.addAction(act_open)

        act_save = QAction("保存工程…", self)
        act_save.triggered.connect(self.save_project)
        bar.addAction(act_save)

        act_dir = QAction("打开工程目录", self)
        act_dir.setToolTip("自动保存的工程都在这里（含标准答案，勿分发）")
        act_dir.triggered.connect(self.open_projects_dir)
        bar.addAction(act_dir)

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
        self._update_window_title()
        if getattr(self, "build_tab", None) is not None:
            self.build_tab.set_current_project(self.project_path)
        if hasattr(self, "act_last"):
            self._refresh_last_action()
        self._schedule_config_check()

    def _update_window_title(self) -> None:
        title = self._window_title()
        if self.project_path:
            title += f" — {self.project_path.name}"
        if self._config_bad:
            title += "  ⚠ 配置有问题"
        self.setWindowTitle(title)

    def _schedule_config_check(self) -> None:
        if not hasattr(self, "_config_timer"):
            self._config_timer = QTimer(self)
            self._config_timer.setSingleShot(True)
            self._config_timer.timeout.connect(self._check_config_now)
        self._config_timer.start(300)

    def _check_config_now(self) -> None:
        bad = bool(self.state.validate())
        if bad != self._config_bad:
            self._config_bad = bad
            self._update_window_title()

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
        path, _ = QFileDialog.getOpenFileName(
            self,
            "打开工程",
            str(project_dir_display()),
            "工程 (*.eesproj *.json);;全部文件 (*)",
        )
        if not path:
            return
        self._load_project(Path(path))

    def save_project(self) -> None:
        start = project_dir_display() / (
            self.project_path.name if self.project_path else "project.eesproj"
        )
        path, selected = QFileDialog.getSaveFileName(
            self,
            "保存工程",
            str(start),
            "工程 (*.eesproj);;JSON (*.json)",
        )
        if not path:
            return
        if not Path(path).suffix:
            # 按老师选的过滤器补扩展名；默认（工程）补 .eesproj
            path += ".json" if "JSON" in (selected or "") else ".eesproj"
        try:
            self.state.save(Path(path))
        except Exception as e:  # noqa: BLE001
            QMessageBox.critical(self, "保存失败", str(e))
            return
        self.project_path = Path(path)
        self._dirty = False
        st = settings()
        st.setValue(LAST_PROJECT, str(path))
        st.setValue(PROJECT_DIR, str(Path(path).parent))
        self._refresh_status()
        self.build_tab.set_project_dir(Path(path).parent)
        self.build_tab.set_current_project(Path(path))
        self.statusBar().showMessage(f"已保存 {path}", 5000)

    # ---------------------------------------------------------------- 工程自动保存/打开

    def _last_project_raw(self) -> Path | None:
        raw = settings().value(LAST_PROJECT, "", type=str)
        return Path(raw) if raw else None

    def _last_project_path(self) -> Path | None:
        p = self._last_project_raw()
        return p if p is not None and p.is_file() else None

    def _last_project_hint(self) -> str:
        p = self._last_project_path()
        return f"；上次工程：{p.name}（工具栏【打开上次工程】）" if p else ""

    def _refresh_last_action(self) -> None:
        p = self._last_project_path()
        self.act_last.setEnabled(p is not None)
        self.act_last.setToolTip(f"上次工程：{p}\n点击打开" if p else "没有可打开的上次工程")

    def open_last_project(self) -> None:
        raw = self._last_project_raw()
        if raw is None:
            QMessageBox.information(self, "没有上次工程", "还没有保存或打开过工程。")
            self._refresh_last_action()
            return
        if not raw.is_file():
            QMessageBox.warning(
                self,
                "上次工程不存在",
                f"上次工程文件已不存在：\n{raw}\n\n可能被移动或删除。",
            )
            self._refresh_last_action()
            return
        self._load_project(raw)

    def _load_project(self, path: Path) -> bool:
        if not self._confirm_discard():
            return False
        try:
            state = ProjectState.load(path)
        except Exception as e:  # noqa: BLE001 - 展示给用户，不吞掉
            QMessageBox.critical(self, "打开失败", f"{path}\n\n{e}")
            return False
        self.state = state
        self.project_path = path
        self._dirty = False
        st = settings()
        st.setValue(LAST_PROJECT, str(path))
        st.setValue(PROJECT_DIR, str(path.parent))
        self._reload_tabs()
        self._refresh_status()
        missing = set(self.state.bank_paths) - set(self.state.banks)
        if missing:
            names = "、".join(sorted(missing))
            self.statusBar().showMessage(
                f"已打开 {path.name}，但 {len(missing)} 个题库源文件读不到（{names}）",
                20000,
            )
            self.setToolTip(f"缺失题库：{names}")
        else:
            self.statusBar().showMessage(f"已打开工程：{path}", 8000)
        return True

    def _on_built(self, out: str) -> None:
        """打包成功后自动保存工程，并把保存位置明确告诉老师。

        **绝不写在考试包输出目录**：那是要发给学生的目录，工程里有标准答案。
        保存目标 = 「工程保存位置」目录；当前工程若在同一目录就写回原文件，
        否则另存一份到该目录（旧文件保留）。
        """
        prod = Path(out)
        if getattr(self, "build_tab", None) is not None:
            self.build_tab.commit_project_dir()
        target_dir = active_projects_dir()
        if self.project_path is not None and self.project_path.parent == target_dir:
            proj = self.project_path
            renamed = False
        else:
            if self.project_path is not None:
                name = self.project_path.name
                renamed = True
            else:
                payload = json.dumps(
                    {
                        "bank_paths": self.state.bank_paths,
                        "students": self.state.students,
                        "sessions": [x.model_dump() for x in self.state.sessions],
                    },
                    ensure_ascii=False,
                    sort_keys=True,
                )
                digest = hashlib.blake2s(payload.encode("utf-8"), digest_size=3).hexdigest()
                name = f"{prod.stem}-{digest}.eesproj"
                renamed = False
            proj = target_dir / name
            if renamed and proj.exists():
                for i in range(1, 1000):
                    cand = proj.with_name(f"{proj.stem}-{i}{proj.suffix}")
                    if not cand.exists():
                        proj = cand
                        break

        try:
            proj.parent.mkdir(parents=True, exist_ok=True)
        except OSError as e:
            self.build_tab.log_line(f"✗ 工程目录不可写：{proj.parent}（{e}）")
            QMessageBox.warning(self, "工程自动保存失败", f"目录不可写：{proj.parent}\n\n{e}")
            return

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
            self.build_tab.log_line(f"✗ 工程保存失败：{proj}（{e}）")
            QMessageBox.warning(self, "工程自动保存失败", f"{proj}\n\n{e}")
            return

        st = settings()
        st.setValue(LAST_PROJECT, str(proj))
        st.setValue(PROJECT_DIR, str(proj.parent))
        self.project_path = proj
        self._dirty = False
        self._refresh_status()
        self.build_tab.set_project_dir(proj.parent)
        self.build_tab.set_current_project(proj)

        note = "（已另存到新目录，旧文件保留）" if renamed else ""
        self.build_tab.log_line(f"✔ 已保存到工程：{proj}{note}")
        self.statusBar().showMessage(f"工程已保存：{proj}", 10000)

        box = QMessageBox(self)
        box.setWindowTitle("工程已保存")
        box.setIcon(QMessageBox.Icon.Information)
        box.setText(f"工程已保存到：\n{proj}{note}")
        box.setInformativeText("⚠ 该文件含标准答案与判分材料，切勿随考试包分发。")
        open_btn = box.addButton("打开目录", QMessageBox.ButtonRole.ActionRole)
        box.addButton("确定", QMessageBox.ButtonRole.AcceptRole)
        box.exec()
        if box.clickedButton() is open_btn:
            from PySide6.QtGui import QDesktopServices

            QDesktopServices.openUrl(QUrl.fromLocalFile(str(proj.parent)))

    def open_projects_dir(self) -> None:
        from PySide6.QtGui import QDesktopServices

        d = active_projects_dir()
        QDesktopServices.openUrl(QUrl.fromLocalFile(str(d)))

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
