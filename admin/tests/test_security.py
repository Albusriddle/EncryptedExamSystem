"""安全与一致性回归测试。

这些用例对应一次全仓审阅发现的问题，**每条都对应一个真实可触发的缺陷**：

1. exam_id 未过滤 → 路径穿越（能把 JSON 写到临时目录之外）；
2. KDF 参数无上限 → 恶意包让客户端申请巨量内存；
3. 场次时间无时区 → 被当成 UTC，整体偏移 8 小时；
4. 同一场次重复引用题库 → UI 说没问题，打包才失败。
"""

from __future__ import annotations

from pathlib import Path

import pytest
from pydantic import ValidationError

from app.core.banks_io import write_temp_banks
from app.core.models import ExamBank, KdfParams, Question, Session, SessionExam
from app.core.state import ProjectState


def _bank(exam_id: str, pick: int = 1) -> ExamBank:
    return ExamBank(
        exam_id=exam_id,
        title="t",
        pick=pick,
        questions=[Question(id="1", type="single", stem="s", options=["a"], answer="a")],
    )


# ---------------------------------------------------------------- 路径穿越


@pytest.mark.parametrize(
    "bad",
    [
        "../../escaped",
        "../x",
        "a/b",
        "a\\b",
        ".hidden",
        "..",
        "",
        "x" * 65,
        "a b",
        "a:b",
    ],
)
def test_exam_id_rejects_path_components(bad: str) -> None:
    """exam_id 会被拼成文件名，必须挡住路径成分与非法字符。"""
    with pytest.raises(ValidationError):
        _bank(bad)


@pytest.mark.parametrize("good", ["demo-math", "A_1.b", "x" * 64, "数学-2", "2024期中"])
def test_exam_id_allows_safe_names(good: str) -> None:
    assert _bank(good).exam_id == good


def test_write_temp_banks_refuses_path_escape(tmp_path: Path) -> None:
    """纵深防御：即使绕过模型校验，写入端也必须拒绝路径成分。

    用 model_construct 跳过校验来模拟"上游被绕过"的情形。
    """
    evil = ExamBank.model_construct(
        exam_id="../../escaped",
        title="t",
        pick=1,
        questions=[Question(id="1", type="single", stem="s", options=["a"])],
    )
    with pytest.raises(ValueError):
        write_temp_banks({"../../escaped": evil}, tmp_path)
    # 确认没有写到 tmp 之外
    assert not (tmp_path.parent.parent / "escaped.json").exists()


def test_write_temp_banks_strips_answer(tmp_path: Path) -> None:
    """临时题库文件里绝不能出现标准答案。"""
    write_temp_banks({"demo-math": _bank("demo-math")}, tmp_path)
    text = (tmp_path / "demo-math.json").read_text(encoding="utf-8")
    assert '"answer"' not in text or '"answer":null' in text.replace(" ", "")
    assert "答案" not in text


# ---------------------------------------------------------------- KDF 上限


def test_kdf_rejects_absurd_m_cost() -> None:
    """m_cost 无上限时，恶意包可让客户端申请数 TB 内存。"""
    with pytest.raises(ValidationError, match="m_cost 过高"):
        KdfParams(m_cost=4_000_000_000)


def test_kdf_rejects_large_t_and_p_cost() -> None:
    with pytest.raises(ValidationError):
        KdfParams(t_cost=99)
    with pytest.raises(ValidationError):
        KdfParams(p_cost=99)


def test_kdf_accepts_production_defaults() -> None:
    k = KdfParams()
    assert (k.m_cost, k.t_cost, k.p_cost) == (65536, 3, 1)


# ---------------------------------------------------------------- 时间必须带时区


def _session(**kw) -> Session:
    base = dict(
        session_id=1,
        start="2026-10-23T11:00:00+08:00",
        end="2026-10-23T16:00:00+08:00",
        students=[202108010217],
        exams=[SessionExam(file="demo-math.json", pick=1)],
    )
    base.update(kw)
    return Session(**base)


def test_time_without_offset_is_rejected() -> None:
    """`2026-10-23 11:00` 会被当成 UTC，教师按北京时间写就整体错 8 小时。"""
    with pytest.raises(ValidationError, match="必须带时区偏移"):
        _session(start="2026-10-23 11:00", end="2026-10-23 16:00")


def test_time_with_offset_is_accepted() -> None:
    _session(start="2026-10-23T11:00:00+08:00", end="2026-10-23T16:00:00Z")
    _session(start="2026-10-23T03:00:00Z", end="2026-10-23T08:00:00Z")


def test_cross_timezone_inverted_window_is_rejected() -> None:
    """03:00Z 开始、10:00+08:00(=02:00Z) 结束 —— 按字符串比较会漏掉。"""
    with pytest.raises(ValidationError, match="开始时间必须早于结束时间"):
        _session(start="2026-10-23T03:00:00Z", end="2026-10-23T10:00:00+08:00")


# ---------------------------------------------------------------- 重复引用题库


def test_duplicate_exam_in_one_session_is_reported(tmp_path: Path) -> None:
    st = ProjectState()
    st.add_bank(_bank("demo-math"), str(tmp_path / "demo-math.json"))
    st.students = [202108010217]
    st.sessions = [
        _session(
            exams=[
                SessionExam(file="demo-math.json", pick=1),
                SessionExam(file="demo-math.json", pick=1),
            ]
        )
    ]
    problems = st.validate()
    assert any("重复引用了题库" in p for p in problems), problems
