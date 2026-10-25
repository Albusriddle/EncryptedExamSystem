"""第二轮全仓审阅发现问题的回归测试。

每条都对应一个**实测可触发**的缺陷：
1. 路径形式的题库引用导致勾选丢失（保存后科目被静默清空）；
2. 全局名单变更不同步到已有场次；
3. 同 exam_id 导入静默替换题库；
4. 水印硬编码，与 branding.json 脱节。
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

pytest.importorskip("PySide6")

from PySide6.QtWidgets import QApplication, QMessageBox  # noqa: E402

from app.branding import branding  # noqa: E402
from app.core.models import ExamBank, Question, Session, SessionExam  # noqa: E402
from app.core.state import ProjectState  # noqa: E402
from app.ui.tab_sessions import SessionsTab  # noqa: E402
from app.ui.tab_students import StudentsTab  # noqa: E402
from app.version import app_version, watermark  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]


@pytest.fixture(autouse=True)
def mute_dialogs(monkeypatch: pytest.MonkeyPatch):
    """弹窗会阻塞测试（实测 8 个用例跑了 129 秒），全部静音。"""
    for name in ("information", "warning", "critical", "question"):
        monkeypatch.setattr(QMessageBox, name, staticmethod(lambda *a, **k: None))


@pytest.fixture(scope="module")
def qapp():
    app = QApplication.instance() or QApplication(sys.argv)
    yield app


def _bank(exam_id: str, title: str = "t", nq: int = 3) -> ExamBank:
    return ExamBank(
        exam_id=exam_id,
        title=title,
        pick=2,
        questions=[
            Question(id=f"q{i}", type="single", stem=f"题 {i}", options=["a", "b"])
            for i in range(nq)
        ],
    )


@pytest.fixture
def state_with_banks() -> ProjectState:
    st = ProjectState()
    for n in ("math", "english"):
        p = ROOT / "banks" / "demo" / f"{n}.json"
        st.add_bank(ExamBank.model_validate_json(p.read_text(encoding="utf-8")), str(p))
    st.students = [202108010217]
    return st


# ---------------------------------------------------------------- 1. 勾选匹配


def test_path_style_file_reference_checks_the_box(qapp, state_with_banks) -> None:
    """`file="demo/math.json"`（Rust 明确支持的形式）必须能勾上。

    旧实现只比 `exam_id` 与 `exam_id.json`，这种引用一个都匹配不上，
    界面显示"没选科目"，教师一保存就把科目清空了。
    """
    exam_id = "demo-math"
    state_with_banks.sessions = [
        Session(
            session_id=1,
            start="2026-10-23T11:00:00+08:00",
            end="2026-10-23T16:00:00+08:00",
            students=[202108010217],
            exams=[SessionExam(file="demo/math.json", pick=2)],
        )
    ]
    tab = SessionsTab(state_with_banks)
    tab._load_into_form(state_with_banks.sessions[0])

    checked = [eid for cb, _, eid in tab.checks if cb.isChecked()]
    assert checked == [exam_id], f"应只勾上 {exam_id}，实际 {checked}"


def test_path_style_reference_survives_save(qapp, state_with_banks) -> None:
    """勾选正确后保存，科目不能丢。"""
    state_with_banks.sessions = [
        Session(
            session_id=1,
            start="2026-10-23T11:00:00+08:00",
            end="2026-10-23T16:00:00+08:00",
            students=[202108010217],
            exams=[SessionExam(file="demo/math.json", pick=2)],
        )
    ]
    tab = SessionsTab(state_with_banks)
    tab._load_into_form(state_with_banks.sessions[0])
    tab.save_session()

    assert len(state_with_banks.sessions[0].exams) == 1, "保存后科目被清空了"


# ---------------------------------------------------------------- 2. 名单同步


def test_roster_change_syncs_all_students_sessions(qapp, monkeypatch) -> None:
    """全局名单增加学生后，使用「全部学生」的场次必须同步。"""
    st = ProjectState()
    st.students = [111, 222]
    st.sessions = [
        Session(
            session_id=1,
            start="2026-10-23T11:00:00+08:00",
            end="2026-10-23T16:00:00+08:00",
            students=[111, 222],
            exams=[SessionExam(file="demo-math.json", pick=1)],
        )
    ]
    tab = StudentsTab(st)
    tab.edit.setPlainText("111\n222\n333")
    tab.apply_to_state()

    assert st.students == [111, 222, 333]
    assert st.sessions[0].students == [111, 222, 333], "场次名单未同步"


def test_roster_change_keeps_custom_sessions(qapp) -> None:
    """自定义名单（不等于全局名单）的场次不能被覆盖。"""
    st = ProjectState()
    st.students = [111, 222]
    st.sessions = [
        Session(
            session_id=1,
            start="2026-10-23T11:00:00+08:00",
            end="2026-10-23T16:00:00+08:00",
            students=[111],
            exams=[SessionExam(file="demo-math.json", pick=1)],
        )
    ]
    tab = StudentsTab(st)
    tab.edit.setPlainText("111\n222\n333")
    tab.apply_to_state()

    assert st.sessions[0].students == [111], "自定义名单不应被改动"


# ---------------------------------------------------------------- 3. 题库冲突


def test_merge_banks_skips_by_default() -> None:
    st = ProjectState()
    st.add_bank(_bank("m", "旧"))
    added, replaced, skipped = st.merge_banks([_bank("m", "新")], overwrite=False)
    assert (added, replaced, skipped) == (0, 0, 1)
    assert st.banks["m"].title == "旧", "不覆盖时不应改变已有题库"


def test_merge_banks_overwrites_when_asked() -> None:
    st = ProjectState()
    st.add_bank(_bank("m", "旧"))
    added, replaced, skipped = st.merge_banks([_bank("m", "新")], overwrite=True)
    assert (added, replaced, skipped) == (0, 1, 0)
    assert st.banks["m"].title == "新"


def test_conflicting_bank_ids_detects_batch_duplicates() -> None:
    st = ProjectState()
    st.add_bank(_bank("existing"))
    conflicts = st.conflicting_bank_ids([_bank("existing"), _bank("new"), _bank("new")])
    assert "existing" in conflicts
    assert "new" in conflicts, "同一批内重复也要报"


# ---------------------------------------------------------------- 4. 水印


def test_watermark_comes_from_branding() -> None:
    b = branding()
    expected = f"{b.abbr} v{app_version()} Powered by {b.credits[0].name}"
    assert watermark() == expected


# ---------------------------------------------------------------- 班级（第十四轮之后）


def test_new_class_and_members_flow(qapp) -> None:
    from app.core.state import ProjectState

    st = ProjectState()
    st.set_students([1, 2])
    tab = StudentsTab(st)
    try:
        st.add_class("计算机 2021-1", "CS2101")
        tab.refresh()
        labels = [tab.list.item(i).text() for i in range(tab.list.count())]
        assert any("计算机 2021-1" in x for x in labels)
        assert any("全部学生" in x for x in labels)

        # 切到班级并保存成员：新学号应自动并入全局名单
        idx = next(i for i in range(tab.list.count()) if "计算机 2021-1" in tab.list.item(i).text())
        tab.list.setCurrentRow(idx)
        tab.edit.setPlainText("1\n2\n3")
        tab.apply_to_state()
        assert st.class_by_name("计算机 2021-1").students == [1, 2, 3]
        assert 3 in st.students
    finally:
        tab.deleteLater()


def test_unassigned_view_is_read_only(qapp) -> None:
    from app.core.models import StudentClass
    from app.core.state import ProjectState

    st = ProjectState()
    st.set_students([1, 2])
    st.classes = [StudentClass(name="一班", students=[1])]
    tab = StudentsTab(st)
    try:
        row = next(i for i in range(tab.list.count()) if "未分班" in tab.list.item(i).text())
        tab.list.setCurrentRow(row)
        assert tab.edit.isReadOnly()
        assert "2" in tab.edit.toPlainText()
    finally:
        tab.deleteLater()


def test_import_disabled_on_unassigned(qapp) -> None:
    from app.core.models import StudentClass
    from app.core.state import ProjectState

    st = ProjectState()
    st.set_students([1, 2])
    st.classes = [StudentClass(name="一班", students=[1])]
    tab = StudentsTab(st)
    try:
        row = next(i for i in range(tab.list.count()) if "未分班" in tab.list.item(i).text())
        tab.list.setCurrentRow(row)
        assert not tab.btn_import.isEnabled(), "只读视图不该允许导入"

        tab.list.setCurrentRow(0)  # 全部学生
        assert tab.btn_import.isEnabled()
    finally:
        tab.deleteLater()


def test_rename_conflict_does_not_save_members(qapp, monkeypatch) -> None:
    """改名冲突时整页都不保存，不能出现'名字没改、成员却写进去'。"""
    from PySide6.QtWidgets import QMessageBox

    from app.core.models import StudentClass
    from app.core.state import ProjectState

    warned: list[str] = []
    monkeypatch.setattr(QMessageBox, "warning", lambda *a, **k: warned.append(a[1]))

    st = ProjectState()
    st.set_students([1, 2])
    st.classes = [
        StudentClass(name="一班", students=[1]),
        StudentClass(name="二班", students=[2]),
    ]
    tab = StudentsTab(st)
    try:
        row = next(i for i in range(tab.list.count()) if "一班" in tab.list.item(i).text())
        tab.list.setCurrentRow(row)
        tab.edit.setPlainText("1\n2\n3")  # 想加 3
        tab.edit_name.setText("二班")  # 但改成重名
        tab.apply_to_state()

        assert warned, "重名必须提示"
        assert st.class_by_name("一班").students == [1], "成员不能被保存"
        assert 3 not in st.students, "全局名单也不能被改动"
    finally:
        tab.deleteLater()


def test_preview_renders_fenced_stem() -> None:
    from app.ui.tab_banks import _format_stem

    out = _format_stem("有下列程序：\n```c\nint x = 1;\n```\n下列说法正确的是（ ）。")
    text = "\n".join(out)
    assert "```" not in text, "围栏不能原样显示"
    assert "| int x = 1;" in text, "代码行要带 | 前缀"
    assert out[0] == "有下列程序："


def test_preview_hides_data_uri_and_bold_markers() -> None:
    from app.ui.tab_banks import _format_stem

    out = "\n".join(
        _format_stem("**重点** 与 `code`\n![图](data:image/png;base64,AAAA)\n| a | b |")
    )
    assert "**" not in out, "加粗标记要剥掉"
    assert "base64" not in out, "预览里绝不能出现 base64"
    assert "[图片 图]" in out


def test_insert_image_button_exists() -> None:
    """题库页必须有「插入图片…」入口，否则教师没法生成 data URI 片段。"""
    from app.core.state import ProjectState
    from app.ui.tab_banks import BanksTab

    tab = BanksTab(ProjectState())
    try:
        assert tab.btn_image.isEnabled()
        assert "插入图片" in tab.btn_image.text()
    finally:
        tab.deleteLater()


def test_preview_renders_option_code_block() -> None:
    from app.ui.tab_banks import _format_stem

    out = "\n".join(_format_stem("程序如下：\n```c\nint x = 1;\n```", prefix="    B. "))
    assert "```" not in out, "选项里的围栏不能原样出现"
    assert "| int x = 1;" in out
    assert out.splitlines()[0] == "    B. 程序如下："
