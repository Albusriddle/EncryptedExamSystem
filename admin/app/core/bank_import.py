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


def answer_letters(answer: Any) -> list[str] | None:
    """把答案统一成字母列表；无法识别返回 None。

    支持三种写法：
    - 结构化：`{"kind": "single", "answer": "B"}` / `{"kind": "multi", "answer": ["A","C"]}`
    - 旧式单字母：`"B"`
    - 旧式多选：`"AB"` / `"A,B"` / `"A B"`

    没有答案（`None` / 空）返回空列表。
    """
    if answer is None:
        return []
    if isinstance(answer, dict):
        kind = answer.get("kind")
        raw = answer.get("answer")
        if kind == "single":
            return [raw] if isinstance(raw, str) and raw else []
        if kind == "multi":
            return [str(x) for x in raw] if isinstance(raw, list) else []
        return None
    if isinstance(answer, str):
        text = answer.strip()
        if not text:
            return []
        parts = [x for x in re.split(r"[,;、\s]+", text) if x]
        if len(parts) > 1:
            return parts
        return list(text)
    return None


def validate_bank_answers(bank: ExamBank) -> list[str]:
    """校验"答案字母是否落在选项里"。返回问题列表（空 = 没问题）。

    为什么需要：原生题库以前只做形状校验，`answer: "E"` 配 A/B 选项也能导入，
    学生永远选不到正确项。外部格式导入时查过，原生格式没查——两条路径必须一致。
    """
    problems: list[str] = []
    for q in bank.questions:
        keys = _option_keys(q)
        if keys is None:
            continue  # 不是选项题（模板/代码/纯文本）
        letters = answer_letters(q.answer)
        if letters is None:
            problems.append(f"题库 {bank.exam_id} 的题 {q.id}：答案格式无法识别（{q.answer!r}）")
            continue
        if not letters:
            continue  # 没有答案不算错：可能只用于展示
        bad = [x for x in letters if x not in keys]
        if bad:
            problems.append(
                f"题库 {bank.exam_id} 的题 {q.id}：答案 {bad} 不在选项 {sorted(keys)} 中"
            )
        elif len(set(letters)) != len(letters):
            problems.append(f"题库 {bank.exam_id} 的题 {q.id}：答案里有重复字母 {letters}")
    return problems


def _option_keys(q: Question) -> set[str] | None:
    """取这道题的合法选项键；不是选项题返回 None。

    要同时支持新格式（`body.kind == "options"`）与旧格式（只有 `options` 数组）：
    旧格式的键由位置生成，与 Rust 端 `model::option_label` 的规则一致。
    """
    if q.body.kind == "options":
        return {o.key for o in q.body.options}
    if q.body.kind == "text" and q.options:
        return {chr(ord("A") + i) if i < 26 else str(i + 1) for i in range(len(q.options))}
    return None


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
    """给题目文件找同类名的答案文件（同目录）。

    先按**文件名**找（`X.answers.json` / `X.answers(1).json`），命中后再校验
    内容确实是答案文件；找不到才退化为扫描。

    旧实现直接遍历同目录所有 `*.json` 并逐个 `json.loads`——一个目录里放几百个
    题库时，导入一个文件会把整个目录解析一遍。
    """
    stem = base_name(path)
    for pattern in (f"{stem}.answers.json", f"{stem}.answers*.json"):
        for cand in sorted(path.parent.glob(pattern)):
            if cand != path and detect(_safe_load(cand)) == "exam_v2_answers":
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
    dup_a = {i for i in a_ids if a_ids.count(i) > 1}
    if dup_a:
        # 重复的答案 id 会让 by_id 静默取最后一条，等于悄悄改答案
        raise BankImportError(f"{a_name}: 答案 ID 重复：{', '.join(sorted(dup_a)[:5])}")
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

    if qtype == "choice":
        keys = {o.key for o in body.options}
        letters = answer_letters(verdict) or []
        if not letters:
            raise BankImportError(f"{where}: 答案为空")
        bad = [x for x in letters if x not in keys]
        if bad:
            raise BankImportError(
                f"{where}: 答案 {bad} 不在选项 {sorted(keys)} 中——学生永远选不到正确项"
            )
        if len(set(letters)) != len(letters):
            raise BankImportError(f"{where}: 多选答案里有重复字母：{letters}")

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
            # 键里带父目录：两个目录下都叫 choice 的题库不能被悄悄合并成一个
            groups.setdefault((str(p.parent.resolve()), base_name(p)), {})[fmt] = (p, payload)
        else:
            errors.append(
                f"{p.name}: 无法识别的格式。\n"
                "  本程序支持：自己的题库（含 exam_id/questions）"
                f"或 format_version={SUPPORTED_FORMAT_VERSION} 的题目/答案文件。"
            )

    seen_exam_ids: dict[str, Path] = {}
    for (parent, key), pair in groups.items():
        q_pair = pair.get("exam_v2_questions")
        a_pair = pair.get("exam_v2_answers")
        if q_pair is None:
            name = a_pair[0].name if a_pair else key
            errors.append(f"{name}: 只有答案文件，缺少同名的 .questions.json")
            continue
        q_path, q_payload = q_pair
        _ = parent  # 仅用于分组键唯一化
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
        if exam_id in seen_exam_ids:
            other = seen_exam_ids[exam_id]
            errors.append(
                f"{q_path.name}: 与 {other.parent.name}/{other.name} 的题库名"
                f"相同（{exam_id!r}）。\n"
                "  请把其中一个文件改名后再导入，否则两个题库会互相覆盖。"
            )
            continue
        seen_exam_ids[exam_id] = q_path
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
        problems = validate_bank_answers(bank)
        if problems:
            errors.append(f"{p.name}:\n" + "\n".join(problems[:5]))
            continue
        loaded.append(ImportedBank(bank=bank, source=p))

    return loaded, errors, warnings


def load_bank_from_path(path: Path) -> ExamBank:
    """从**任意支持格式**的文件加载一个题库（供工程重开时使用）。

    工程文件里只存 `bank_paths`，不存题库内容——教师会持续修改源文件。
    但外部题库的源文件是 `X.questions.json`（不是本程序的 ExamBank 形状），
    所以重开工程时必须走同一套转换逻辑，否则题库会被静默丢弃。

    异常统一用 `BankImportError`，调用方负责展示。
    """
    payload = _safe_load(path)
    if payload is None:
        raise BankImportError(f"{path.name}: 不是合法的 UTF-8 JSON")

    fmt = detect(payload)
    if fmt == "native":
        return ExamBank.model_validate(payload)

    if fmt == "exam_v2_answers":
        raise BankImportError(f"{path.name}: 这是答案文件，工程里应记录题目文件")

    if fmt != "exam_v2_questions":
        raise BankImportError(f"{path.name}: 无法识别的题库格式")

    answers = _sibling_answers(path)
    if answers is None:
        raise BankImportError(f"{path.name}: 找不到同名答案文件（{base_name(path)}.answers.json）")

    base = base_name(path)
    item = convert_pair(
        payload,
        _safe_load(answers) or {},
        exam_id=validate_exam_id(base),
        title=base,
        q_name=path.name,
        a_name=answers.name,
    )
    return item.bank
