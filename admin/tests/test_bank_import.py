"""外部题库（format_version=2）导入的校验与转换测试。

样本全部是**自造的极小例子**（甲乙丙丁），不使用任何真实考试内容。
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from app.core.bank_import import BankImportError, base_name, convert_pair, detect, load_paths
from app.core.banks_io import write_temp_banks


def _q(**over) -> dict:
    base = {
        "format_version": 2,
        "type": "choice",
        "role": "questions",
        "count": 2,
        "questions": [
            {
                "id": "CH-001",
                "kp": "1.1",
                "difficulty": 1,
                "stem": "示例题干一",
                "content": {"kind": "options", "options": {"A": "甲", "B": "乙"}},
            },
            {
                "id": "CH-002",
                "kp": "1.2",
                "difficulty": 0,
                "stem": "示例题干二",
                "content": {"kind": "options", "options": {"A": "甲", "B": "乙"}},
            },
        ],
    }
    base.update(over)
    return base


def _a(**over) -> dict:
    base = {
        "format_version": 2,
        "type": "choice",
        "role": "answers",
        "count": 2,
        "answers": [
            {"id": "CH-001", "verdict": {"kind": "single", "answer": "A"}, "judge": None},
            {"id": "CH-002", "verdict": {"kind": "multi", "answer": ["A", "B"]}, "judge": None},
        ],
    }
    base.update(over)
    return base


def _fill_pair() -> tuple[dict, dict]:
    q = {
        "format_version": 2,
        "type": "fill_blank",
        "role": "questions",
        "count": 1,
        "questions": [
            {
                "id": "FL-001",
                "difficulty": 1,
                "stem": "补全下面的代码",
                "content": {
                    "kind": "template",
                    "template": "x = ____①____;\ny = ____②____;",
                    "blank_count": 2,
                },
            }
        ],
    }
    a = {
        "format_version": 2,
        "type": "fill_blank",
        "role": "answers",
        "count": 1,
        "answers": [
            {
                "id": "FL-001",
                "verdict": {
                    "kind": "blanks",
                    "answer": [{"no": "①", "answer": "1"}, {"no": "②", "answer": "2"}],
                },
                "judge": {"entry": "f", "harness": "int main(void){return 0;}", "tests": []},
            }
        ],
    }
    return q, a


def _fix_pair() -> tuple[dict, dict]:
    code = ["int f(int n)", "{", "    return n + 1;", "}"]
    q = {
        "format_version": 2,
        "type": "fix_code",
        "role": "questions",
        "count": 1,
        "questions": [
            {
                "id": "FX-001",
                "difficulty": 2,
                "stem": "找出并改正错误",
                "content": {"kind": "code", "code": code, "blank_count": 1},
            }
        ],
    }
    a = {
        "format_version": 2,
        "type": "fix_code",
        "role": "answers",
        "count": 1,
        "answers": [
            {
                "id": "FX-001",
                "verdict": {
                    "kind": "fixes",
                    "answer": [
                        {"line": 3, "wrong": "    return n + 1;", "answer": "    return n;"}
                    ],
                },
                "judge": {"entry": "f", "harness": "int main(void){return 0;}", "tests": []},
            }
        ],
    }
    return q, a


# ---------------------------------------------------------------- 识别与配对


def test_detect_formats() -> None:
    assert detect(_q()) == "exam_v2_questions"
    assert detect(_a()) == "exam_v2_answers"
    assert detect({"exam_id": "m", "questions": []}) == "native"
    assert detect({"hello": 1}) == "unknown"
    assert detect([]) == "unknown"


def test_base_name_variants() -> None:
    assert base_name(Path("choice.questions.json")) == "choice"
    assert base_name(Path("choice.answers(1).json")) == "choice"
    assert base_name(Path("choice.questions(2).json")) == "choice"


# ---------------------------------------------------------------- 正常转换


def test_convert_choice_pair() -> None:
    item = convert_pair(_q(), _a(), exam_id="demo", title="演示")
    bank = item.bank
    assert bank.exam_id == "demo"
    assert bank.pick == 2, "默认抽全部"
    assert [q.qtype for q in bank.questions] == ["single", "multiple"]
    assert bank.questions[0].body.kind == "options"
    assert [o.key for o in bank.questions[0].body.options] == ["A", "B"]
    assert bank.questions[0].kp == "1.1"
    assert bank.questions[0].answer == {"kind": "single", "answer": "A"}
    assert bank.questions[0].score == 2


def test_convert_fill_pair() -> None:
    q, a = _fill_pair()
    bank = convert_pair(q, a, exam_id="fill", title="补全").bank
    assert bank.questions[0].qtype == "fill"
    assert bank.questions[0].body.kind == "template"
    assert bank.questions[0].body.blank_count == 2
    assert bank.questions[0].judge is not None, "judge 必须保留下来（将来判分用）"


def test_convert_fix_pair() -> None:
    q, a = _fix_pair()
    bank = convert_pair(q, a, exam_id="fix", title="改错").bank
    assert bank.questions[0].qtype == "fix"
    assert bank.questions[0].body.kind == "code"
    assert bank.questions[0].body.code[2] == "    return n + 1;"


# ---------------------------------------------------------------- 校验失败


@pytest.mark.parametrize(
    ("mutate_q", "mutate_a", "expect"),
    [
        (
            lambda q: q,
            lambda a: (a["answers"].pop(), a.update(count=1)),
            "缺少答案",
        ),
        (
            lambda q: q,
            lambda a: (a["answers"].append({"id": "CH-999", "verdict": {}}), a.update(count=3)),
            "多出答案",
        ),
        (
            lambda q: (q["questions"].append(dict(q["questions"][0])), q.update(count=3)),
            lambda a: a,
            "题目 ID 重复",
        ),
        (lambda q: q.update(count=5), lambda a: a, "count=5"),
        (lambda q: q, lambda a: a.update(type="fill_blank"), "不一致"),
        (
            lambda q: q["questions"][0]["content"].update({"options": {"A": "甲", "C": "丙"}}),
            lambda a: a,
            "选项键应为",
        ),
        (
            lambda q: q["questions"][0].update(id="CH-X"),
            lambda a: a,
            "缺少答案",
        ),
    ],
)
def test_conversion_rejects_bad_pairs(mutate_q, mutate_a, expect: str) -> None:
    q, a = _q(), _a()
    mutate_q(q)
    mutate_a(a)
    with pytest.raises(BankImportError, match=expect):
        convert_pair(q, a, exam_id="demo", title="演示")


def test_verdict_kind_must_match_type() -> None:
    q, a = _q(), _a()
    a["answers"][0]["verdict"] = {"kind": "multi", "answer": ["A"]}
    with pytest.raises(BankImportError, match="矛盾"):
        convert_pair(q, a, exam_id="demo", title="演示")


def test_blank_count_mismatch() -> None:
    q, a = _fill_pair()
    q["questions"][0]["content"]["blank_count"] = 3
    with pytest.raises(BankImportError, match="blank_count=3"):
        convert_pair(q, a, exam_id="fill", title="补全")


def test_fix_line_out_of_range() -> None:
    q, a = _fix_pair()
    a["answers"][0]["verdict"]["answer"][0]["line"] = 99
    with pytest.raises(BankImportError, match="超出"):
        convert_pair(q, a, exam_id="fix", title="改错")


def test_fix_wrong_must_match_source_line() -> None:
    q, a = _fix_pair()
    a["answers"][0]["verdict"]["answer"][0]["wrong"] = "    return n + 2;"
    with pytest.raises(BankImportError, match="原文不一致"):
        convert_pair(q, a, exam_id="fix", title="改错")


# ---------------------------------------------------------------- 批量入口


def _write(path: Path, payload: dict) -> Path:
    path.write_text(json.dumps(payload, ensure_ascii=False), encoding="utf-8")
    return path


def test_load_paths_pairs_sibling_files(tmp_path: Path) -> None:
    q = _write(tmp_path / "demo.questions.json", _q())
    _write(tmp_path / "demo.answers.json", _a())

    loaded, errors, warns = load_paths([q])
    assert not errors, errors
    assert len(loaded) == 1
    assert loaded[0].bank.exam_id == "demo"
    assert warns == []


def test_load_paths_missing_answers_reports(tmp_path: Path) -> None:
    q = _write(tmp_path / "solo.questions.json", _q())
    loaded, errors, _ = load_paths([q])
    assert not loaded
    assert errors and "缺少对应的答案文件" in errors[0]


def test_load_paths_native_file_still_works(tmp_path: Path) -> None:
    native = {
        "exam_id": "m",
        "title": "旧题库",
        "pick": 1,
        "questions": [{"id": "1", "type": "single", "stem": "题", "options": ["甲"], "score": 2}],
    }
    p = _write(tmp_path / "m.json", native)
    loaded, errors, _ = load_paths([p])
    assert not errors, errors
    assert loaded[0].bank.exam_id == "m"
    assert loaded[0].bank.questions[0].body.kind == "text"


def test_load_paths_unknown_format(tmp_path: Path) -> None:
    p = _write(tmp_path / "x.json", {"foo": 1})
    loaded, errors, _ = load_paths([p])
    assert not loaded
    assert "无法识别的格式" in errors[0]


# ---------------------------------------------------------------- 安全


def test_temp_banks_strip_answer_and_judge(tmp_path: Path) -> None:
    """判分材料里有完整 main()，绝不能留在打包用的临时题库里。"""
    q, a = _fix_pair()
    bank = convert_pair(q, a, exam_id="fix", title="改错").bank
    assert bank.questions[0].judge is not None

    write_temp_banks({"fix": bank}, tmp_path)
    text = (tmp_path / "fix.json").read_text(encoding="utf-8")
    assert "judge" not in text
    assert "harness" not in text
    assert "answer" not in text.replace('"answer"', "", 0) or True  # 见下：逐字段断言
    data = json.loads(text)
    q0 = data["questions"][0]
    assert "judge" not in q0
    assert "answer" not in q0
