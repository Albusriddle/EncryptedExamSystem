"""判分界面：生成成绩、双击改分、导出四个 CSV。"""

from __future__ import annotations

import csv
import shutil
from pathlib import Path

import pytest

pytest.importorskip("PySide6")

from PySide6.QtWidgets import QFileDialog, QMessageBox  # noqa: E402

from app.core.models import ExamBank, Question  # noqa: E402
from app.core.state import ProjectState  # noqa: E402
from app.ui.tab_answers import AnswersTab  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
WORK = ROOT / "target" / "test-grading-ui"


@pytest.fixture(scope="module")
def qapp():
    import sys

    from PySide6.QtWidgets import QApplication

    app = QApplication.instance() or QApplication(sys.argv)
    yield app


@pytest.fixture()
def tab(qapp, monkeypatch, tmp_path):
    WORK.mkdir(parents=True, exist_ok=True)
    bank = ExamBank(
        exam_id="m",
        title="数学",
        pick=3,
        questions=[
            Question(
                id="q1",
                type="single",
                stem="1+1?",
                answer={"kind": "single", "answer": "A"},
                score=2,
            ),
            Question(
                id="q2",
                type="multiple",
                stem="偶数?",
                answer={"kind": "multi", "answer": ["A", "C"]},
                score=3,
            ),
            Question(
                id="q3",
                type="fill",
                stem="填空",
                answer={"kind": "blanks", "answer": [{"no": "①", "answer": "x"}]},
                score=4,
            ),
        ],
    )
    state = ProjectState(salt="00" * 16, students=[1])
    state.banks["m"] = bank

    t = AnswersTab(state)
    fake = WORK / "answers-1-1-00000000.eea"
    fake.write_bytes(b"x")
    t.files = [fake]
    t.results = {
        str(fake): {
            "ok": True,
            "sheet": {
                "student_id": 1,
                "session_id": 1,
                "qids": ["q1", "q2", "q3"],
                "answers": ["A", "AC", '["y"]'],
            },
        }
    }
    monkeypatch.setattr(QMessageBox, "information", staticmethod(lambda *a, **k: None))
    monkeypatch.setattr(QMessageBox, "warning", staticmethod(lambda *a, **k: None))
    yield t
    t.deleteLater()


def test_grade_fills_table_and_summary(tab) -> None:
    tab._grade()
    assert tab.grade_table.rowCount() == 3
    assert len(tab.grade_rows) == 3
    assert tab.grade_rows[0].status == "correct"
    assert tab.grade_rows[2].status == "unjudged", "默认不判填空"
    assert "已判 2 题" in tab.grade_summary.text()


def test_manual_score_edit_updates_total(tab) -> None:
    tab._grade()
    before = tab.grade_summary.text()
    tab.grade_table.item(1, 7).setText("1")  # 第二题（多选）本来满分 3
    assert tab.grade_rows[1].manual_score == 1
    assert "3 → 1" in tab.grade_table.item(1, 9).text()
    assert tab.grade_summary.text() != before


def test_invalid_score_is_clamped(tab) -> None:
    tab._grade()
    tab.grade_table.item(0, 7).setText("999")
    assert tab.grade_rows[0].final_score == 2, "不能超过满分"
    tab.grade_table.item(0, 7).setText("-5")
    assert tab.grade_rows[0].final_score == 0


def test_changing_master_clears_old_grades(tab, monkeypatch) -> None:
    """改主密钥/换包后，旧成绩必须清空——否则会导出与当前输入不符的分数。"""
    tab._grade()
    assert tab.grade_table.rowCount() > 0
    assert tab.btn_grade_csv.isEnabled()

    tab.master.setText("00" * 32)  # 触发 _invalidate

    assert tab.grade_rows == []
    assert tab.grade_table.rowCount() == 0
    assert not tab.btn_grade_csv.isEnabled()
    assert "尚未判分" in tab.grade_summary.text()


def test_export_writes_four_csvs(tab, monkeypatch) -> None:
    tab._grade()
    out = WORK / "out"
    # 每次清空：固定目录复用会让上一轮的 CSV 混进断言（真实踩过）
    shutil.rmtree(out, ignore_errors=True)
    out.mkdir(parents=True, exist_ok=True)
    monkeypatch.setattr(QFileDialog, "getExistingDirectory", staticmethod(lambda *a, **k: str(out)))
    tab._export_grading()

    names = {p.name for p in out.glob("*.csv")}
    assert len(names) == 5, names
    assert any(n.startswith("已判分-明细-") for n in names)
    assert any(n.startswith("已判分-汇总-") for n in names)
    assert any(n.startswith("未判分-") for n in names)
    assert any(n.startswith("全量-") for n in names)
    assert any(n.startswith("考生成绩表-") for n in names), "应导出按学号的成绩表"
    assert (out / "说明.txt").is_file(), "口径说明应单独成文件"

    detail = next(out.glob("已判分-明细-*.csv"))
    rows = list(csv.reader(detail.read_text(encoding="utf-8-sig").splitlines()))
    assert rows[0][0] == "学号"
    assert "调整时间" in rows[0]


def test_essay_manual_score_moves_to_judged_export(tab, monkeypatch) -> None:
    """问答题：打完分应进入"已判分"，未判分里不再出现，且带题干摘要。"""
    from app.core.models import Question

    tab.state.banks["m"].questions.append(
        Question(
            id="q9",
            type="essay",
            stem="谈谈你对加密的理解",
            answer=None,
            score=10,
        )
    )
    tab.results[str(tab.files[0])]["sheet"]["qids"].append("q9")
    tab.results[str(tab.files[0])]["sheet"]["answers"].append("这是我的回答")

    tab._grade()
    row = next(r for r in tab.grade_rows if r.qid == "q9")
    assert row.status == "unjudged" and row.max_score == 10

    idx = tab.grade_rows.index(row)
    tab.grade_table.item(idx, 7).setText("7")
    assert tab.grade_rows[idx].manual_score == 7

    out = WORK / "out-essay"
    shutil.rmtree(out, ignore_errors=True)
    out.mkdir(parents=True, exist_ok=True)
    monkeypatch.setattr(QFileDialog, "getExistingDirectory", staticmethod(lambda *a, **k: str(out)))
    tab._export_grading()

    detail = next(out.glob("已判分-明细-*.csv")).read_text(encoding="utf-8-sig")
    assert "q9" in detail and "7" in detail
    unjudged = next(out.glob("未判分-*.csv")).read_text(encoding="utf-8-sig")
    assert "q9" not in unjudged
    assert "题干摘要" in unjudged.splitlines()[0]


def test_score_table_is_sorted_and_grouped_by_type(tab, monkeypatch) -> None:
    """考生成绩表：按学号排序、按题型分项、未判计入满分。"""
    import csv as _csv

    tab._grade()
    out = WORK / "out-score"
    shutil.rmtree(out, ignore_errors=True)
    out.mkdir(parents=True, exist_ok=True)
    monkeypatch.setattr(QFileDialog, "getExistingDirectory", staticmethod(lambda *a, **k: str(out)))
    tab._export_grading()

    path = next(out.glob("考生成绩表-*.csv"))
    rows = list(_csv.reader(path.read_text(encoding="utf-8-sig").splitlines()))
    header, data = rows[0], rows[1:]
    assert header[:3] == ["学号", "班级", "场次"]
    assert "单选题得分" in header and "单选题满分" in header
    assert "总得分" in header and "成绩状态" in header
    assert [int(r[0]) for r in data] == sorted(int(r[0]) for r in data)
    # q3 是填空题但默认不判 → 未判题数 1，状态"含未判"
    assert data[0][header.index("未判题数")] == "1"
    assert data[0][header.index("成绩状态")] == "含未判"


def test_zero_score_on_unjudged_question_is_recorded(tab, monkeypatch) -> None:
    """未判题打 0 分也必须生效（曾经被当成"没改"而丢弃）。"""
    from app.core.models import Question

    tab.state.banks["m"].questions.append(
        Question(id="q0", type="essay", stem="一点没写对", answer=None, score=10)
    )
    tab.results[str(tab.files[0])]["sheet"]["qids"].append("q0")
    tab.results[str(tab.files[0])]["sheet"]["answers"].append("答非所问")

    tab._grade()
    idx = next(i for i, r in enumerate(tab.grade_rows) if r.qid == "q0")
    tab.grade_table.item(idx, 7).setText("0")

    row = tab.grade_rows[idx]
    assert row.manual_score == 0, "0 分是有效的人工评分"
    assert tab._status_text(row) == "人工判分"
    assert "人工 0" in tab.grade_table.item(idx, 9).text()
    from app.core.grading import is_graded

    assert is_graded(row), "人工给的 0 分应计入已判"
