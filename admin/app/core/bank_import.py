"""外部题库格式（format_version=2）的识别、配对与转换。

## 这批文件的形状

题目与答案**分成两个文件**，靠 `id` 对齐：

```json
// X.questions.json
{"format_version": 2, "type": "choice", "role": "questions", "count": 500,
 "questions": [{"id": "CH-001", "kp": "1.1", "difficulty": 1,
                "stem": "…", "content": {"kind": "options", "options": {"A": "4", …}}}]}

// X.answers.json
{"format_version": 2, "type": "choice", "role": "answers", "count": 500,
 "answers": [{"id": "CH-001", "verdict": {"kind": "single", "answer": "A"},
              "judge": null}]}
```

`content.kind` 有三种：`options`（选择）、`template`（补全）、`code`（改错）。
`verdict.kind` 对应：`single`/`multi`、`blanks`、`fixes`。

## 为什么单独一层

- **识别只认字段，不认文件名**：`format_version` + `role` 是声明式契约；
  文件名只是用来配对（`X.questions.json` ↔ `X.answers.json`）。
- **转换即校验**：题目要发给学生考试，少一个选项、错一个答案现场没法补救，
  所以宁可导入时拒绝，也不让脏数据进包。
- **Qt-free**：本模块不导入 PySide6，测试可直接调用。

## 错误信息约定

每条错误都以 `<文件名> 第 N 条（id=…）: ` 开头，说明"哪里错、怎么改"。
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from app.core.models import ChoiceOption, ExamBank, Question, QuestionBody, validate_exam_id

#: 外部格式版本
SUPPORTED_FORMAT_VERSION = 2

#: 每种 type 允许的 verdict.kind
_VERDICT_KINDS = {
    "choice": {"single", "multi"},
    "fill_blank": {"blanks"},
    "fix_code": {"fixes"},
}

#: 补全题占位符：____①____ 形式
_BLANK_RE = re.compile(r"____([①②③④⑤⑥⑦⑧⑨⑩])____")

#: 下载重复带来的后缀：(1)、(2) …
_DUP_SUFFIX_RE = re.compile(r"\s*\(\d+\)$")


class BankImportError(Exception):
    """导入失败（带用户可读说明）。"""


@dataclass
class ImportedBank:
    """一个转换好的题库及其来源。"""

    bank: ExamBank
    source: Path
    #: 非致命问题（例如 judge 缺失），导入后提示教师
    warnings: list[str] = field(default_factory=list)


# ---------------------------------------------------------------- 识别与配对


def detect(payload: Any) -> str:
    """识别 JSON 的格式：`exam_v2_questions` / `exam_v2_answers` / `native` / `unknown`。"""
    if not isinstance(payload, dict):
        return "unknown"
    if payload.get("format_version") == SUPPORTED_FORMAT_VERSION:
        role = payload.get("role")
        if role == "questions":
            return "exam_v2_questions"
        if role == "answers":
            return "exam_v2_answers"
        return "unknown"
    if "exam_id" in payload and "questions" in payload:
        return "native"
    return "unknown"


def base_name(path: Path) -> str:
    """`choice.questions(1).json` → `choice`。"""
    name = path.name
    for suffix in (".json",):
        if name.lower().endswith(suffix):
            name = name[: -len(suffix)]
    name = _DUP_SUFFIX_RE.sub("", name)
    for suffix in (".questions", ".answers"):
        if name.lower().endswith(suffix):
            name = name[: -len(suffix)]
    return name


def _sibling_answers(path: Path) -> Path | None:
    """给题目文件找同类名的答案文件（同目录）。"""
    stem = base_name(path)
    for cand in path.parent.glob("*.json"):
        if (
            base_name(cand) == stem
            and cand != path
            and detect(_safe_load(cand)) == "exam_v2_answers"
        ):
            return cand
    return None


def _safe_load(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8-sig"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError):
        return None


# ---------------------------------------------------------------- 转换


def convert_pair(
    questions_payload: dict,
    answers_payload: dict,
    *,
    exam_id: str,
    title: str,
    default_score: int = 2,
    q_name: str = "",
    a_name: str = "",
) -> ImportedBank:
    """把一对 questions/answers 合并成内部题库，并做完整校验。"""
    qtype = str(questions_payload.get("type", ""))
    if qtype not in _VERDICT_KINDS:
        raise BankImportError(
            f"{q_name}: 不支持的题型 type={qtype!r}（支持 choice/fill_blank/fix_code）"
        )
    if answers_payload.get("type") != qtype:
        raise BankImportError(
            f"{a_name}: type={answers_payload.get('type')!r} 与题目文件的 {qtype!r} 不一致"
        )

    qs = questions_payload.get("questions")
    ans = answers_payload.get("answers")
    if not isinstance(qs, list) or not isinstance(ans, list):
        raise BankImportError(f"{q_name}: 缺少 questions/answers 数组")

    _check_count(q_name, questions_payload.get("count"), len(qs), "questions")
    _check_count(a_name, answers_payload.get("count"), len(ans), "answers")

    # id 必须一一对应
    q_ids = [str(x.get("id", "")) for x in qs]
    a_ids = [str(x.get("id", "")) for x in ans]
    dup = {i for i in q_ids if q_ids.count(i) > 1}
    if dup:
        raise BankImportError(f"{q_name}: 题目 ID 重复：{', '.join(sorted(dup)[:5])}")
    missing = set(q_ids) - set(a_ids)
    orphan = set(a_ids) - set(q_ids)
    if missing or orphan:
        parts = []
        if missing:
            parts.append(f"缺少答案：{', '.join(sorted(missing)[:5])}")
        if orphan:
            parts.append(f"多出答案：{', '.join(sorted(orphan)[:5])}")
        raise BankImportError(f"{a_name}: " + "；".join(parts))

    by_id = {str(x["id"]): x for x in ans}
    questions: list[Question] = []
    warnings: list[str] = []
    for idx, raw in enumerate(qs):
        qid = str(raw.get("id", ""))
        where = f"{q_name} 第 {idx + 1} 条（id={qid}）"
        answer = by_id[qid]
        q = _convert_one(raw, answer, qtype, where=where, default_score=default_score)
        if q.judge is None and qtype in {"fill_blank", "fix_code"}:
            warnings.append(f"{where}: 没有 judge 判分材料，将来无法自动判分")
        questions.append(q)

    bank = ExamBank(
        exam_id=exam_id,
        title=title,
        pick=len(questions),  # 默认抽全部；教师可在场次里改小
        questions=questions,
    )
    return ImportedBank(bank=bank, source=Path(q_name or exam_id), warnings=warnings)


def _check_count(name: str, declared: Any, actual: int, field_name: str) -> None:
    if declared is not None and int(declared) != actual:
        raise BankImportError(f"{name}: count={declared}，实际 {field_name} 有 {actual} 条")


def _convert_one(
    raw: dict,
    ans: dict,
    qtype: str,
    *,
    where: str,
    default_score: int,
) -> Question:
    qid = str(raw.get("id", "")).strip()
    if not qid:
        raise BankImportError(f"{where}: id 为空")
    stem = str(raw.get("stem", "")).strip()
    if not stem:
        raise BankImportError(f"{where}: 题干为空")

    content = raw.get("content") or {}
    kind = content.get("kind")
    verdict = ans.get("verdict") or {}
    vkind = verdict.get("kind")
    if vkind not in _VERDICT_KINDS[qtype]:
        allowed = sorted(_VERDICT_KINDS[qtype])
        raise BankImportError(f"{where}: 题型 {qtype} 的答案 kind 应为 {allowed}，实际 {vkind!r}")

    difficulty = raw.get("difficulty")
    if difficulty is not None:
        try:
            difficulty = int(difficulty)
        except (TypeError, ValueError):
            raise BankImportError(f"{where}: difficulty 必须是整数，实际 {difficulty!r}") from None

    if qtype == "choice":
        # 外部题库用 difficulty==0 表示多选题——这是**题型信息的一部分**，
        # 与答案 kind 必须互相印证；不一致说明数据被改坏了。
        want_multi = difficulty == 0
        is_multi = vkind == "multi"
        if want_multi != is_multi:
            raise BankImportError(
                f"{where}: difficulty={difficulty}（{'多选' if want_multi else '单选'}）"
                f"与答案 kind={vkind!r} 矛盾"
            )

    body, internal_type = _build_body(qtype, kind, content, difficulty, verdict, where)

    return Question(
        id=qid,
        type=internal_type,
        stem=stem,
        body=body,
        kp=str(raw["kp"]) if raw.get("kp") is not None else None,
        difficulty=difficulty,
        answer=verdict,
        judge=ans.get("judge"),
        score=default_score,
    )


def _build_body(
    qtype: str,
    kind: Any,
    content: dict,
    difficulty: int | None,
    verdict: dict,
    where: str,
) -> tuple[QuestionBody, str]:
    if qtype == "choice":
        if kind != "options":
            raise BankImportError(f"{where}: 选择题的 content.kind 应为 options，实际 {kind!r}")
        options = content.get("options")
        if not isinstance(options, dict) or not options:
            raise BankImportError(f"{where}: content.options 必须是非空对象")
        keys = list(options.keys())
        expected = [chr(ord("A") + i) for i in range(len(keys))]
        if keys != expected:
            raise BankImportError(f"{where}: 选项键应为 {expected}，实际 {keys}")
        if difficulty == 0:
            internal = "multiple"
        else:
            internal = "single"
        return (
            QuestionBody(
                kind="options",
                options=[ChoiceOption(key=k, text=str(v)) for k, v in options.items()],
            ),
            internal,
        )

    if qtype == "fill_blank":
        if kind != "template":
            raise BankImportError(f"{where}: 补全题的 content.kind 应为 template，实际 {kind!r}")
        template = str(content.get("template", ""))
        blanks = int(content.get("blank_count", 0))
        found = len(_BLANK_RE.findall(template))
        if blanks <= 0:
            raise BankImportError(f"{where}: blank_count 必须 ≥ 1")
        if found != blanks:
            raise BankImportError(
                f"{where}: 模板里有 {found} 个 ____①____ 占位符，但 blank_count={blanks}"
            )
        answers = verdict.get("answer")
        if not isinstance(answers, list) or len(answers) != blanks:
            got = len(answers) if isinstance(answers, list) else "非数组"
            raise BankImportError(f"{where}: 有 {got} 个空答案，但 blank_count={blanks}")
        return QuestionBody(kind="template", template=template, blank_count=blanks), "fill"

    # fix_code
    if kind != "code":
        raise BankImportError(f"{where}: 改错题的 content.kind 应为 code，实际 {kind!r}")
    code = content.get("code")
    if not isinstance(code, list) or not code:
        raise BankImportError(f"{where}: content.code 必须是非空数组")
    blanks = int(content.get("blank_count", 0))
    if blanks <= 0:
        raise BankImportError(f"{where}: blank_count 必须 ≥ 1")

    fixes = verdict.get("answer")
    if not isinstance(fixes, list):
        raise BankImportError(f"{where}: verdict.answer 必须是数组")
    if len(fixes) != blanks:
        raise BankImportError(f"{where}: 有 {len(fixes)} 条改正项，但 blank_count={blanks}")
    lines = [str(x) for x in code]
    for i, fix in enumerate(fixes):
        if not isinstance(fix, dict):
            raise BankImportError(f"{where}: 第 {i + 1} 条改正项不是对象")
        line_no = fix.get("line")
        if not isinstance(line_no, int) or not (1 <= line_no <= len(lines)):
            raise BankImportError(
                f"{where}: 第 {i + 1} 条改正项的 line={line_no!r} 超出 1..{len(lines)}"
            )
        wrong = fix.get("wrong")
        if not isinstance(wrong, str) or not wrong:
            raise BankImportError(f"{where}: 第 {i + 1} 条改正项缺少 wrong 原文")
        if wrong != lines[line_no - 1]:
            raise BankImportError(
                f"{where}: 第 {i + 1} 条改正项的 wrong 与第 {line_no} 行原文不一致\n"
                f"    原文: {lines[line_no - 1]!r}\n"
                f"    wrong: {wrong!r}"
            )
        if not isinstance(fix.get("answer"), str) or not fix["answer"]:
            raise BankImportError(f"{where}: 第 {i + 1} 条改正项缺少 answer")

    return QuestionBody(kind="code", code=lines, blank_count=blanks), "fix"


# ---------------------------------------------------------------- 批量入口


def load_paths(paths: list[Path]) -> tuple[list[ImportedBank], list[str], list[str]]:
    """把用户选中的一批文件解析成题库。

    返回 `(题库, 错误列表, 提示列表)`。错误是"这个文件不能导入"的说明，
    提示是"导入了但不完美"（例如缺 judge 判分材料）。
    """
    loaded: list[ImportedBank] = []
    errors: list[str] = []
    warnings: list[str] = []

    groups: dict[str, dict[str, tuple[Path, dict]]] = {}
    natives: list[Path] = []

    for p in paths:
        payload = _safe_load(p)
        if payload is None:
            errors.append(
                f"{p.name}: 不是合法的 UTF-8 JSON。\n"
                "  请用支持 UTF-8 的编辑器另存（记事本选「UTF-8」；\n"
                "  VS Code 右下角点编码 → Save with Encoding → UTF-8）。"
            )
            continue
        fmt = detect(payload)
        if fmt == "native":
            natives.append(p)
        elif fmt in ("exam_v2_questions", "exam_v2_answers"):
            groups.setdefault(base_name(p), {})[fmt] = (p, payload)
        else:
            errors.append(
                f"{p.name}: 无法识别的格式。\n"
                "  本程序支持：自己的题库（含 exam_id/questions）"
                f"或 format_version={SUPPORTED_FORMAT_VERSION} 的题目/答案文件。"
            )

    for key, pair in groups.items():
        q_pair = pair.get("exam_v2_questions")
        a_pair = pair.get("exam_v2_answers")
        if q_pair is None:
            name = a_pair[0].name if a_pair else key
            errors.append(f"{name}: 只有答案文件，缺少同名的 .questions.json")
            continue
        q_path, q_payload = q_pair
        if a_pair is None:
            sibling = _sibling_answers(q_path)
            if sibling is None:
                errors.append(f"{q_path.name}: 缺少对应的答案文件（{key}.answers.json）")
                continue
            a_path, a_payload = sibling, _safe_load(sibling)
        else:
            a_path, a_payload = a_pair

        try:
            exam_id = validate_exam_id(key)
        except ValueError as e:
            errors.append(f"{q_path.name}: 题库名 {key!r} 不能用作文件名——{e}")
            continue
        try:
            item = convert_pair(
                q_payload,
                a_payload or {},
                exam_id=exam_id,
                title=key,
                q_name=q_path.name,
                a_name=a_path.name,
            )
        except BankImportError as e:
            errors.append(str(e))
            continue
        item.source = q_path
        loaded.append(item)
        warnings.extend(item.warnings)

    for p in natives:
        try:
            bank = ExamBank.model_validate_json(p.read_text(encoding="utf-8-sig"))
        except Exception as e:  # noqa: BLE001 - pydantic 的 ValidationError
            errors.append(f"{p.name}:\n{e}")
            continue
        loaded.append(ImportedBank(bank=bank, source=p))

    return loaded, errors, warnings
