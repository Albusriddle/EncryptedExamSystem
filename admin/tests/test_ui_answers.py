"""答题卡标签页的显示逻辑（离屏）。"""

from __future__ import annotations

from pathlib import Path

import pytest

pytest.importorskip("PySide6")

from PySide6.QtWidgets import QApplication  # noqa: E402

from app.core.state import ProjectState  # noqa: E402
from app.ui.tab_answers import AnswersTab, collect_eea  # noqa: E402


@pytest.fixture(scope="module")
def qapp():
    app = QApplication.instance() or QApplication([])
    yield app


def _sheet(**over):
    base = {
        "student_id": 2021001,
        "session_id": 1,
        "qids": ["q1", "q2", "q3"],
        "answers": ["B", "AC", None],
        "submitted_unix": 1_700_000_000,
    }
    base.update(over)
    return base


def test_verified_row_is_filled(qapp) -> None:
    tab = AnswersTab(ProjectState())
    try:
        f = Path("answers-2021001-1.eea")
        tab._append([f])
        tab._on_one(str(f), {"ok": True, "sheet": _sheet()})
        assert tab.table.item(0, 1).text() == "2021001"
        assert tab.table.item(0, 3).text() == "3"
        assert tab.table.item(0, 4).text() == "2"
        assert tab.table.item(0, 5).text() == "1"
        assert "通过" in tab.table.item(0, 7).text()
        assert tab.table.item(0, 6).text() != "未交卷"
    finally:
        tab.deleteLater()


def test_failed_row_shows_reason(qapp) -> None:
    tab = AnswersTab(ProjectState())
    try:
        f = Path("bad.eea")
        tab._append([f])
        tab._on_one(str(f), {"ok": False, "message": "解密失败（密钥不匹配或文件被改动）"})
        assert "失败" in tab.table.item(0, 7).text()
        assert "解密失败" in tab.table.item(0, 8).text()
    finally:
        tab.deleteLater()


def test_summary_counts(qapp) -> None:
    tab = AnswersTab(ProjectState())
    try:
        a, b = Path("a.eea"), Path("b.eea")
        tab._append([a, b])
        tab._on_one(str(a), {"ok": True, "sheet": _sheet()})
        tab._on_one(str(b), {"ok": False, "message": "x"})
        tab._on_all()
        assert "1 通过" in tab.status.text() and "1 失败" in tab.status.text()
    finally:
        tab.deleteLater()


def test_same_filename_in_different_dirs_does_not_collide(qapp, tmp_path) -> None:
    """两台机器会产出同名文件，按文件名索引会互相覆盖。"""
    a = tmp_path / "a" / "answers-2021001-1.eea"
    b = tmp_path / "b" / "answers-2021001-1.eea"
    a.parent.mkdir()
    b.parent.mkdir()

    tab = AnswersTab(ProjectState())
    try:
        tab._append([a, b])
        tab._on_one(str(a), {"ok": True, "sheet": _sheet(student_id=111)})
        tab._on_one(str(b), {"ok": True, "sheet": _sheet(student_id=222)})
        assert len(tab.results) == 2
        assert tab.table.item(0, 1).text() == "111"
        assert tab.table.item(1, 1).text() == "222"
    finally:
        tab.deleteLater()


def test_append_clears_previous_results(qapp) -> None:
    tab = AnswersTab(ProjectState())
    try:
        a, b = Path("a.eea"), Path("b.eea")
        tab._append([a])
        tab._on_one(str(a), {"ok": True, "sheet": _sheet()})
        assert tab.results
        tab._append([b])
        assert tab.results == {}, "换文件后旧结果必须清掉，否则导出会混进旧数据"
    finally:
        tab.deleteLater()


# ---------------------------------------------------------------- 子进程解码（v0.3.1 回归）


def test_run_tool_decodes_utf8_output() -> None:
    """中文 Windows 默认用 GBK 解码子进程输出，会把 UTF-8 中文解坏。

    这是 v0.3.1 的真实故障：教师输入正确 master 也"解不开"，
    实际是解码线程抛异常后 stdout 变空，界面只显示"无输出"。
    """
    import os
    import sys

    from app.ui.tab_answers import run_tool

    cmd = [
        sys.executable,
        "-c",
        "import sys;sys.stdout.buffer.write('答题卡 ok'.encode('utf-8'))",
    ]
    out, err, code = run_tool(cmd, dict(os.environ))
    assert code == 0
    assert "答题卡" in out, f"输出被解码坏了：{out!r}（stderr={err!r}）"


def test_verify_worker_reports_builder_error_in_chinese(tmp_path) -> None:
    """真实调用 exam-builder：必须把它的中文报错原样带回来，而不是"无输出"。"""
    from app.ui.tab_answers import VerifyWorker
    from app.ui.tab_build import builder_exe

    exe = builder_exe()
    if not exe.is_file():
        pytest.skip("本机没有 exam-builder 产物（CI 的 python job 不编译 Rust）")

    missing = tmp_path / "nope.eea"
    worker = VerifyWorker([missing], "00" * 32, None)
    result = worker._verify_one(missing)
    assert result["ok"] is False
    assert "无输出" not in result["message"], "解码失败时不能只报'无输出'"
    assert "读取" in result["message"] or "失败" in result["message"]


def test_binding_status_is_explicit(qapp) -> None:
    """没给包时必须显示"未校验绑定"，不能只说"通过"。"""
    tab = AnswersTab(ProjectState())
    try:
        a, b = Path("a.eea"), Path("b.eea")
        tab._append([a, b])
        tab._on_one(str(a), {"ok": True, "binding": True, "sheet": _sheet()})
        tab._on_one(str(b), {"ok": True, "binding": None, "sheet": _sheet()})
        assert "已绑定" in tab.table.item(0, 7).text()
        assert "未校验绑定" in tab.table.item(1, 7).text()
    finally:
        tab.deleteLater()


def test_failure_row_shows_dash_not_zero(qapp) -> None:
    tab = AnswersTab(ProjectState())
    try:
        f = Path("bad.eea")
        tab._append([f])
        tab._on_one(str(f), {"ok": False, "message": "解密失败"})
        assert tab.table.item(0, 3).text() == "—"
        assert tab.table.item(0, 1).text() == "—"
    finally:
        tab.deleteLater()


def test_results_invalidated_when_master_changes(qapp) -> None:
    tab = AnswersTab(ProjectState())
    try:
        f = Path("a.eea")
        tab._append([f])
        tab._on_one(str(f), {"ok": True, "binding": True, "sheet": _sheet()})
        assert tab.results
        tab.master.setText("11" * 32)
        assert tab.results == {}, "换了主密钥，旧结果必须作废"
        assert "重新校验" in tab.status.text()
    finally:
        tab.deleteLater()


def test_collect_eea_expands_directory(tmp_path) -> None:

    d = tmp_path / "收上来的"
    d.mkdir()
    (d / "a.eea").write_bytes(b"x")
    (d / "b.eea").write_bytes(b"x")
    (d / "说明.txt").write_text("ignore", encoding="utf-8")
    out = collect_eea([d, d / "说明.txt"])
    assert [p.name for p in out] == ["a.eea", "b.eea"]


# ---------------------------------------------------------------- 校验期间冻结列表


class _FakeRunningWorker:
    """假的"正在运行"worker：只为验证 UI 的冻结逻辑。"""

    class _Sig:
        def connect(self, *_a, **_k) -> None:
            pass

    def __init__(self, *_a, **_k) -> None:
        self.one_done = self._Sig()
        self.all_done = self._Sig()

    def isRunning(self) -> bool:  # noqa: N802 - QThread 接口
        return True

    def start(self) -> None:
        pass


def test_list_is_frozen_while_verifying(qapp, monkeypatch) -> None:
    import app.ui.tab_answers as mod

    monkeypatch.setattr(mod, "VerifyWorker", _FakeRunningWorker)
    tab = AnswersTab(ProjectState())
    try:
        f = Path("a.eea")
        tab._append([f])
        tab.master.setText("11" * 32)
        tab._start()

        assert not tab.btn_add.isEnabled()
        assert not tab.btn_clear.isEnabled()
        tab._append([Path("b.eea")])
        assert [p.name for p in tab.files] == ["a.eea"], "校验中不允许改列表"
        tab._clear()
        assert tab.files, "校验中不允许清空列表"
    finally:
        tab.worker = None
        tab.deleteLater()


def test_start_is_not_reentrant(qapp, monkeypatch) -> None:
    import app.ui.tab_answers as mod

    monkeypatch.setattr(mod, "VerifyWorker", _FakeRunningWorker)
    tab = AnswersTab(ProjectState())
    try:
        tab._append([Path("a.eea")])
        tab.master.setText("11" * 32)
        tab._start()
        first = tab.worker
        tab._start()
        assert tab.worker is first, "校验进行中不能再次启动"
    finally:
        tab.worker = None
        tab.deleteLater()


def test_master_and_package_frozen_while_verifying(qapp, monkeypatch) -> None:
    import app.ui.tab_answers as mod

    monkeypatch.setattr(mod, "VerifyWorker", _FakeRunningWorker)
    tab = AnswersTab(ProjectState())
    try:
        tab._append([Path("a.eea")])
        tab.master.setText("11" * 32)
        tab._start()
        assert not tab.master.isEnabled(), "校验中不能改主密钥"
        assert not tab.pkg.isEnabled(), "校验中不能换包"
        # 即便有人绕过禁用直接改文本，也不能把结果清掉
        tab.results["x"] = {"ok": True}
        tab.master.setText("22" * 32)
        assert "x" in tab.results, "忙时 _invalidate 必须直接返回"
    finally:
        tab.worker = None
        tab.deleteLater()


def test_attendance_button_enabled_after_verify(qapp) -> None:
    tab = AnswersTab(ProjectState())
    try:
        assert not tab.btn_absent.isEnabled()
        tab.results = {"x": {"ok": True}}
        tab._on_all()
        assert tab.btn_absent.isEnabled()
    finally:
        tab.deleteLater()


def test_attendance_dialog_lists_missing(qapp) -> None:
    from app.core.models import Session, SessionExam
    from app.ui.attendance_dialog import AttendanceDialog

    sess = Session(
        session_id=1,
        start="2026-12-20T09:00:00+08:00",
        end="2026-12-20T11:00:00+08:00",
        students=[1, 2],
        exams=[SessionExam(file="m.json", pick=1)],
    )
    recs = [
        {
            "file": "a.eea",
            "ok": True,
            "binding": None,
            "sheet": {"student_id": 1, "session_id": 1, "submitted": True},
        }
    ]
    dlg = AttendanceDialog(None, [sess], recs, [])
    try:
        assert dlg.table.rowCount() == 2
        assert "缺考 1" in dlg.summary.text()
    finally:
        dlg.deleteLater()
