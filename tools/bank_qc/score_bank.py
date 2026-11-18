#!/usr/bin/env python3
"""难度评分器：``S = 0.35×K + 0.45×A + 0.20×C``，按固定阈值分档。

口径（与 docs/出题质检与难度口径.md 一致）
------------------------------------------------------------------
* K 知识点数量 1–3（权重 35%）；A 算法难度与数量 1–4（45%）；C 代码行数 1–4（20%）；
* 单选 / 判断 / 填空 / 改错：``S < 1.90 → 1``；``< 2.55 → 2``；否则 3；
* 大题（``type=code``）：``S < 1.75 → 1``；``< 2.60 → 2``；``< 3.30 → 3``；否则 4；
* 多选：JSON 里 ``difficulty`` **固定 9**（哨兵值，不参与 1–3 统计），
  真实 S 必须落在 ``[2.00, 2.20]``；
* 大题默认走 ``uniform`` 模式：**统一难度 3、统一 15 分**，
  此时 S 只作为质量抽检参考，不参与分档（``--big-mode formula`` 可切回 1–4 分档）。

K/A/C 的来源
------------
1. ``meta.scoring = {"k":…, "a":…, "c":…}`` —— 作者标注，优先级最高；
2. 没有标注时按启发式**估算**（``source=heuristic``）：只能当占位，
   A=4（设计类）与"核心算法行数"这类判断必须人工给，工具给不出来。

用法
----
::

    python score_bank.py final                     # 只统计（dry-run，不写文件）
    python score_bank.py final --write             # 回写 difficulty（多选强制 9）
    python score_bank.py final --write --backup    # 回写前留一份 .bak-<时间>
    python score_bank.py final --strict            # 有偏差就退出码 1（CI 用）

写盘边界（故意收窄）
--------------------
* ``--write`` **默认只改** ``difficulty`` 与 ``meta.scoring``；大题的 ``score`` /
  ``rubric`` 合计属于考务口径（统一 15 分、总分 450/库），要改得显式加 ``--fix-score``；
* 有题缺 ``meta.scoring``、只能靠启发式估算时，``--write`` **直接拒绝**（退出码 3）：
  把猜测写进 ``difficulty`` 等于把估算洗成"作者标注"。确实接受就加 ``--allow-heuristic``，
  写进去的 ``meta.scoring`` 会带 ``source: heuristic``，下次仍会被标出来；
* ``--strict`` 与 ``--write`` 互斥（strict 是只读校验）。

退出码：``0`` 正常；``1`` ``--strict`` 下存在偏差；``2`` 用法 / IO 问题；
``3`` ``--write`` 被拒绝（缺标注）。
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import time
from collections import Counter
from collections.abc import Iterable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

# ---------------------------------------------------------------- 口径常量

K_WEIGHT = 0.35
A_WEIGHT = 0.45
C_WEIGHT = 0.20

#: 分档阈值一律用 **S×100 的整数**（190 = 1.90）。浮点在 2.55 / 3.30 这类边界上
#: 会被二进制表示带偏，同一分可能因为求值顺序落到不同档，整数不吃这个亏。
THRESHOLDS_NEAR = (190, 255)  # 单选 / 判断 / 填空 / 改错
THRESHOLDS_BIG = (175, 260, 330)  # 大题（formula 模式）
MULTIPLE_SENTINEL = 9
MULTIPLE_S_RANGE = (200, 220)  # ×100

BIG_LEVEL_SCORES = {1: 10, 2: 15, 3: 15, 4: 25}

ALLOWED_TYPES = ("single", "multiple", "judge", "fill", "fix", "essay", "code")
TYPE_ALIASES = {"multi": "multiple", "blank": "fill", "blanks": "fill", "fix_code": "fix"}

FENCE_RE = re.compile(r"```([^\n`]*)\n?(.*?)```", re.S)
FENCE_LANGS = {
    "",
    "c",
    "cpp",
    "c++",
    "python",
    "py",
    "java",
    "rust",
    "go",
    "js",
    "javascript",
    "ts",
    "typescript",
    "sql",
    "bash",
    "sh",
    "text",
    "plain",
    "none",
}

#: K 的九个"知识领域"关键词（与 docs/出题质检与难度口径.md 的 K 定义一致）
K_CATEGORIES = {
    "数组": r"\[[^\]]*\]|数组",
    "字符串": r"\"|'|strlen|strcpy|strcmp|strcat|字符串",
    "函数": r"\b[A-Za-z_]\w*\s*\([^;]*\)\s*\{|函数",
    "指针": r"\*\s*[A-Za-z_]|&\s*[A-Za-z_]|指针",
    "结构体": r"\bstruct\b|结构体",
    "宏": r"#\s*define|宏",
    "文件": r"FILE|fopen|fprintf|fscanf|文件",
    "位运算": r"<<|>>|\^|~|按位",
    "算法": r"排序|查找|递归|二分|遍历|模拟|动态规划",
}

LOOP_RE = re.compile(r"\b(for|while|do)\b")
BRANCH_RE = re.compile(r"\b(if|else|switch|case)\b")
NESTED_LOOP_RE = re.compile(
    r"for\s*\([^)]*\)\s*\{?[^}]*for\s*\(|while\s*\([^)]*\)\s*\{?[^}]*while\s*\(", re.S
)
RECURSION_RE = re.compile(r"(\w+)\s*\([^;]*\)\s*\{[^}]*\1\s*\(", re.S)
INCLUDE_RE = re.compile(r"^\s*#\s*include")
MAIN_RE = re.compile(r"^\s*(?:int|void)\s+main\s*\(")


# ---------------------------------------------------------------- 数据结构


@dataclass
class QuestionPlan:
    qid: str
    qtype: str
    k: int
    a: int
    c: int
    source: str
    s: float
    s100: int
    target: int
    current: Any
    note: str = ""


@dataclass
class BankPlan:
    label: str
    path: Path
    questions: list[dict]
    plans: list[QuestionPlan] = field(default_factory=list)

    @property
    def changes(self) -> list[QuestionPlan]:
        return [p for p in self.plans if p.current != p.target]


class UsageError(Exception):
    pass


# ---------------------------------------------------------------- 基础工具


def force_utf8_when_piped() -> None:
    """重定向到文件/管道时按 UTF-8 输出（中文 Windows 默认是 cp936）。"""
    for stream in (sys.stdout, sys.stderr):
        try:
            if not stream.isatty():
                stream.reconfigure(encoding="utf-8", errors="replace")  # type: ignore[attr-defined]
        except (AttributeError, ValueError):
            pass


def is_int(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def normalize_type(raw: Any) -> str:
    text = str(raw or "").strip()
    return TYPE_ALIASES.get(text, text)


def iter_fences(stem: str) -> Iterable[str]:
    for match in FENCE_RE.finditer(stem or ""):
        lang, body = match.group(1).strip(), match.group(2)
        if lang.lower() not in FENCE_LANGS:
            body = lang + body
        yield body


def code_of(q: dict) -> str:
    """取出这道题里所有可数行的代码：围栏块 + 模板 + 逐行代码 + 参考答案。"""
    body = q.get("body") if isinstance(q.get("body"), dict) else {}
    chunks: list[str] = []
    kind = body.get("kind")
    if kind == "code":
        chunks.append("\n".join(str(x) for x in (body.get("code") or [])))
    elif kind == "template":
        chunks.append(str(body.get("template") or ""))
    chunks.extend(iter_fences(str(q.get("stem") or "")))
    answer = q.get("answer") if isinstance(q.get("answer"), dict) else {}
    reference = answer.get("reference")
    if isinstance(reference, str) and reference.strip():
        chunks.append(reference)
    return "\n".join(chunk for chunk in chunks if chunk)


def core_code_lines(text: str) -> int:
    """非空代码行数，去掉 include/main 行、纯花括号与 ``return 0;`` 样板。"""
    count = 0
    for line in text.splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        if stripped in ("{", "}"):
            continue
        if INCLUDE_RE.match(line) or MAIN_RE.match(line) or stripped == "return 0;":
            continue
        count += 1
    return count


# ---------------------------------------------------------------- 打分


def s100(k: int, a: int, c: int) -> int:
    """S×100 的整数形式：``35K + 45A + 20C``。"""
    return int(round(100 * (K_WEIGHT * k + A_WEIGHT * a + C_WEIGHT * c)))


def s_value(k: int, a: int, c: int) -> float:
    """只用于显示（``S=2.80``）。判档一律用 :func:`s100`。"""
    return s100(k, a, c) / 100


def near_difficulty(v100: int) -> int:
    low, high = THRESHOLDS_NEAR
    if v100 < low:
        return 1
    if v100 < high:
        return 2
    return 3


def big_difficulty(v100: int) -> int:
    first, second, third = THRESHOLDS_BIG
    if v100 < first:
        return 1
    if v100 < second:
        return 2
    if v100 < third:
        return 3
    return 4


def c_bucket(lines: int) -> int:
    if lines <= 3:
        return 1
    if lines <= 8:
        return 2
    if lines <= 20:
        return 3
    return 4


def heuristic_scoring(q: dict) -> tuple[int, int, int, str]:
    """没有 meta.scoring 时的粗略估算。**不替代作者标注**。"""
    text = str(q.get("stem") or "")
    code = code_of(q)
    haystack = text + "\n" + code
    categories = sum(1 for pattern in K_CATEGORIES.values() if re.search(pattern, haystack))
    k = max(1, min(3, categories))
    c = c_bucket(core_code_lines(code))

    loops = len(LOOP_RE.findall(code))
    branches = len(BRANCH_RE.findall(code))
    nested = bool(NESTED_LOOP_RE.search(code))
    recursion = bool(RECURSION_RE.search(code))
    if loops == 0 and branches == 0:
        a = 1
    elif nested or recursion or loops >= 2:
        a = 3
    else:
        a = 2
    note = "启发式估算（A 不会给 4，核心算法行数需人工确认）"
    return k, a, c, note


def scoring_of(q: dict) -> tuple[int, int, int, str, str]:
    """返回 (k, a, c, source, note)。"""
    meta = q.get("meta")
    if isinstance(meta, dict):
        scoring = meta.get("scoring")
        if isinstance(scoring, dict):
            values = [scoring.get("k"), scoring.get("a"), scoring.get("c")]
            if all(is_int(v) for v in values):
                k, a, c = (int(v) for v in values)
                if 1 <= k <= 3 and 1 <= a <= 4 and 1 <= c <= 4:
                    # 之前用 --write 落盘的启发式值要一直带着 "heuristic" 标记，
                    # 否则重跑一次就变成了"作者已标注"，等于把估算洗成事实。
                    recorded = str(scoring.get("source") or "").strip().lower()
                    if recorded.startswith("heuristic"):
                        return k, a, c, "heuristic", "meta.scoring 标着 heuristic，仍需人工确认"
                    return k, a, c, "meta", ""
                hk, ha, hc, hnote = heuristic_scoring(q)
                return (
                    hk,
                    ha,
                    hc,
                    "heuristic",
                    f"meta.scoring 越界（k={k} a={a} c={c}），已改用启发式；{hnote}",
                )
    k, a, c, note = heuristic_scoring(q)
    return k, a, c, "heuristic", note


def target_difficulty(
    qtype: str, v100: int, *, big_mode: str, big_uniform: int
) -> tuple[int, str]:
    """返回 (目标 difficulty, 说明)。``v100`` 是 S×100 的整数。"""
    if qtype == "multiple":
        low, high = MULTIPLE_S_RANGE
        note = ""
        if not (low <= v100 <= high):
            note = (
                f"真实 S={v100 / 100:.2f} 不在 [{low / 100:.2f}, {high / 100:.2f}]，"
                "请重写为统一结构（不要靠改 difficulty 抹平）"
            )
        return MULTIPLE_SENTINEL, note
    if qtype == "code":
        if big_mode == "formula":
            return big_difficulty(v100), "formula 模式：按大题 1.75/2.60/3.30 分档"
        return big_uniform, ""
    return near_difficulty(v100), ""


# ---------------------------------------------------------------- 读写


def load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8-sig"))
    except OSError as exc:
        raise UsageError(f"读不了 {path}：{exc}") from exc
    except UnicodeDecodeError as exc:
        raise UsageError(f"{path} 不是 UTF-8：{exc}（另存为 UTF-8 后重试）") from exc
    except json.JSONDecodeError as exc:
        raise UsageError(f"{path} 不是合法 JSON：第 {exc.lineno} 行 {exc.msg}") from exc


def write_json(path: Path, data: dict) -> None:
    text = json.dumps(data, ensure_ascii=False, indent=2) + "\n"
    path.write_text(text, encoding="utf-8", newline="\n")


def discover(paths: list[Path], recursive: bool) -> list[Path]:
    files: list[Path] = []
    for raw in paths:
        path = raw.expanduser()
        if not path.exists():
            raise UsageError(f"路径不存在：{raw}")
        if path.is_dir():
            pattern = "**/*.json" if recursive else "*.json"
            files.extend(sorted(p for p in path.glob(pattern) if p.is_file()))
        else:
            files.append(path)
    unique: list[Path] = []
    seen: set[Path] = set()
    for path in files:
        key = path.resolve()
        if key in seen:
            continue
        seen.add(key)
        unique.append(path)
    return unique


# ---------------------------------------------------------------- 计划


def is_legacy(payload: Any) -> bool:
    return isinstance(payload, dict) and payload.get("format_version") == 2


def plan_bank(path: Path, payload: dict, *, big_mode: str, big_uniform: int) -> BankPlan:
    label = path.stem
    questions = payload.get("questions")
    if not isinstance(questions, list):
        raise UsageError(f"{path}: 缺少 questions 数组")
    bank = BankPlan(label=label, path=path, questions=questions)
    for index, q in enumerate(questions):
        if not isinstance(q, dict):
            continue
        qtype = normalize_type(q.get("type"))
        if qtype not in ALLOWED_TYPES:
            raise UsageError(
                f"{path} 第 {index + 1} 条：type={q.get('type')!r} 不在 "
                f"{', '.join(ALLOWED_TYPES)}（旧别名会被自动迁移）"
            )
        k, a, c, source, note = scoring_of(q)
        v100 = s100(k, a, c)
        target, extra = target_difficulty(qtype, v100, big_mode=big_mode, big_uniform=big_uniform)
        notes = [x for x in (note, extra) if x]
        bank.plans.append(
            QuestionPlan(
                qid=str(q.get("id") or f"#{index + 1}"),
                qtype=qtype,
                k=k,
                a=a,
                c=c,
                source=source,
                s=s_value(k, a, c),
                s100=v100,
                target=target,
                current=q.get("difficulty"),
                note="；".join(notes),
            )
        )
    return bank


def apply_plan(
    q: dict,
    plan: QuestionPlan,
    *,
    big_mode: str,
    big_score: int,
    write_meta: bool,
    fix_score: bool,
) -> None:
    """把目标写回这一题。

    默认**只动 `difficulty` 与 `meta.scoring`**：分值和 `rubric` 合计属于考务口径
    （大题统一 15 分、总分 450/库），顺手改会让"跑一次评分器"变成"悄悄改了卷面分数"。
    确实要把大题分值改回统一口径时才加 ``--fix-score``。
    """
    q["difficulty"] = plan.target
    if write_meta:
        meta = q.get("meta")
        if not isinstance(meta, dict):
            meta = {}
            q["meta"] = meta
        meta["scoring"] = {
            "k": plan.k,
            "a": plan.a,
            "c": plan.c,
            "source": plan.source if plan.source == "meta" else "heuristic",
        }
    if not fix_score or plan.qtype != "code":
        return
    new_score = big_score if big_mode == "uniform" else BIG_LEVEL_SCORES[plan.target]
    q["score"] = new_score
    answer = q.get("answer")
    if not isinstance(answer, dict):
        return
    answer["total"] = new_score
    points = answer.get("points")
    if isinstance(points, list) and points:
        subtotal = sum(int(p.get("score", 0)) for p in points if isinstance(p, dict))
        delta = new_score - subtotal
        if delta:
            for point in reversed(points):
                if isinstance(point, dict):
                    point["score"] = max(0, int(point.get("score", 0)) + delta)
                    break


# ---------------------------------------------------------------- 输出


def fmt_dist(counter: Counter) -> str:
    if not counter:
        return "—"
    keys = sorted(counter, key=lambda x: (0, x) if isinstance(x, int) else (1, str(x)))
    return " ".join(f"{key}:{counter[key]}" for key in keys)


def score_drift(bank: BankPlan, big_mode: str, big_score: int) -> list[str]:
    """大题分值偏离统一口径的题（``--fix-score`` 才修；默认只报告）。"""
    if big_mode != "uniform":
        return []
    return [
        str(q.get("id") or f"#{i + 1}")
        for i, q in enumerate(bank.questions)
        if isinstance(q, dict)
        and normalize_type(q.get("type")) == "code"
        and q.get("score") != big_score
    ]


def report_bank(
    bank: BankPlan, max_report: int, big_mode: str, big_uniform: int, big_score: int
) -> None:
    current = Counter(p.current for p in bank.plans)
    target = Counter(p.target for p in bank.plans)
    types = Counter(p.qtype for p in bank.plans)
    sources = Counter(p.source for p in bank.plans)
    print(f"== {bank.label}  ({bank.path})")
    print(f"   题数 {len(bank.plans)}（{fmt_dist(types)}）")
    if types.get("code"):
        if big_mode == "uniform":
            print(f"   大题口径 uniform：统一难度 {big_uniform}、统一 {big_score} 分")
        else:
            print("   大题口径 formula：1.75/2.60/3.30 分档，分值 10/15/15/25")
    print(f"   当前难度 {fmt_dist(current)}")
    print(f"   应为难度 {fmt_dist(target)}")
    print(f"   来源 {fmt_dist(sources)}")
    multis = [p for p in bank.plans if p.qtype == "multiple"]
    if multis:
        low, high = MULTIPLE_S_RANGE
        hit = sum(1 for p in multis if low <= p.s100 <= high)
        s_vals = sorted({f"{p.s:.2f}" for p in multis})
        print(
            f"   多选 {len(multis)} 题：真实 S {s_vals}，"
            f"落在 [{low / 100:.2f}, {high / 100:.2f}] 的 {hit}/{len(multis)}"
        )
    changes = bank.changes
    if not changes:
        print("   结论：与 K/A/C 口径一致，无需改动")
    else:
        hint = "（多选一律写 9）" if any(p.qtype == "multiple" for p in changes) else ""
        print(f"   需改 {len(changes)} 题{hint}")
        for plan in changes[:max_report]:
            print(
                f"     · {plan.qid} {plan.current!r} → {plan.target}  "
                f"S={plan.s:.2f}（k={plan.k} a={plan.a} c={plan.c}, {plan.source}）"
            )
        if len(changes) > max_report:
            print(f"     … 其余 {len(changes) - max_report} 题省略")
    noted = [plan for plan in bank.plans if plan.note]
    for plan in noted[:max_report]:
        print(f"   注意 {plan.qid}: {plan.note}")
    if len(noted) > max_report:
        print(f"   … 另有 {len(noted) - max_report} 条注意事项省略")
    drift = score_drift(bank, big_mode, big_score)
    if drift:
        print(
            f"   注意：{len(drift)} 道大题分值不是 {big_score} 分（{', '.join(drift[:5])}…）；"
            "加 --fix-score 才回写，默认不动考务数字"
        )
    heuristics = sum(1 for p in bank.plans if p.source.startswith("heuristic"))
    if heuristics:
        print(
            f"   提示：{heuristics} 题的 K/A/C 来自启发式（不是作者标注）；"
            "请人工补 meta.scoring 后重跑"
        )
    print()


# ---------------------------------------------------------------- 入口


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="score_bank.py",
        description=(
            "按 S = 0.35K + 0.45A + 0.20C 计算难度目标并（可选）回写 difficulty。"
            " 默认只统计，不写文件。"
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "示例：\n"
            "  python score_bank.py banks/final\n"
            "  python score_bank.py banks/final --write --backup\n"
            "  python score_bank.py banks/final --strict --json\n"
            "  python score_bank.py banks/final --big-mode formula\n"
        ),
    )
    parser.add_argument("paths", nargs="*", metavar="PATH", help="题库文件或目录；默认当前目录")
    parser.add_argument("--recursive", action="store_true", help="目录递归查找 *.json")
    parser.add_argument("--write", action="store_true", help="把目标难度写回题库文件")
    parser.add_argument("--backup", action="store_true", help="写入前留一份 <文件名>.bak-<时间>")
    parser.add_argument(
        "--strict", action="store_true", help="存在偏差时退出码 1（CI 用；不写文件）"
    )
    parser.add_argument(
        "--big-mode",
        choices=("uniform", "formula"),
        default="uniform",
        help="大题口径：uniform=统一难度/分值（默认）；formula=按 1/2/3/4 分档",
    )
    parser.add_argument(
        "--big-difficulty",
        type=int,
        default=3,
        help="uniform 模式下大题的固定难度（默认 3）",
    )
    parser.add_argument(
        "--big-score", type=int, default=15, help="uniform 模式下大题的固定分值（默认 15）"
    )
    parser.add_argument(
        "--no-meta", action="store_true", help="--write 时不写 meta.scoring（只改 difficulty）"
    )
    parser.add_argument(
        "--allow-heuristic",
        action="store_true",
        help="允许把启发式估算的结果写进 difficulty（默认拒绝，避免把猜测写成事实）",
    )
    parser.add_argument(
        "--fix-score",
        action="store_true",
        help="--write 时顺带把大题 score 与 rubric 合计改成统一口径（默认不动考务数字）",
    )
    parser.add_argument(
        "--max-report", type=int, default=20, help="每库最多列出多少条改动（默认 20）"
    )
    parser.add_argument("--json", action="store_true", help="输出 JSON 报告")
    return parser


def main(argv: list[str] | None = None) -> int:
    force_utf8_when_piped()
    parser = build_parser()
    args = parser.parse_args(argv)
    if args.write and args.strict:
        print("参数错误：--write 与 --strict 不能同时用（strict 是只读校验）", file=sys.stderr)
        return 2
    if not (1 <= args.big_difficulty <= 4):
        print("参数错误：--big-difficulty 必须在 1–4", file=sys.stderr)
        return 2
    if not (0 <= args.big_score <= 1000):
        print("参数错误：--big-score 必须在 0–1000", file=sys.stderr)
        return 2

    try:
        files = discover([Path(p) for p in (args.paths or ["."])], args.recursive)
    except UsageError as exc:
        print(f"错误：{exc}", file=sys.stderr)
        return 2
    if not files:
        print("错误：给定路径下没有找到 *.json 题库文件", file=sys.stderr)
        return 2

    plans: list[BankPlan] = []
    skipped: list[str] = []
    for path in files:
        try:
            payload = load_json(path)
        except UsageError as exc:
            print(f"错误：{exc}", file=sys.stderr)
            return 2
        if is_legacy(payload):
            skipped.append(f"{path.name}: 旧格式（format_version=2）没有 meta/score，先转原生 JSON")
            continue
        if not isinstance(payload, dict) or "questions" not in payload:
            skipped.append(f"{path.name}: 不是题库（缺 questions），已跳过")
            continue
        try:
            plans.append(
                plan_bank(
                    path,
                    payload,
                    big_mode=args.big_mode,
                    big_uniform=args.big_difficulty,
                )
            )
        except UsageError as exc:
            print(f"错误：{exc}", file=sys.stderr)
            return 2

    if not plans:
        print("错误：没有可评分的题库", file=sys.stderr)
        for line in skipped:
            print(f"  · {line}", file=sys.stderr)
        return 2

    total_questions = sum(len(bank.plans) for bank in plans)
    total_changes = sum(len(bank.changes) for bank in plans)
    total_drift = sum(len(score_drift(bank, args.big_mode, args.big_score)) for bank in plans)
    total_heuristic = sum(
        1 for bank in plans for plan in bank.plans if plan.source.startswith("heuristic")
    )

    if args.write and total_heuristic and not args.allow_heuristic:
        # 启发式只是"猜"，把它写进 difficulty 就等于把猜测洗成事实：
        # 下次再跑，meta.scoring 里已经有整数了，读的人分不出是标注还是估算。
        print(
            f"拒绝写盘：有 {total_heuristic} 题没有 meta.scoring，只能给启发式估算。\n"
            "  处理办法：补齐 meta.scoring 后重跑；确实接受估算值就加 --allow-heuristic"
            "（会写 source=heuristic，下次仍会被标出来）。",
            file=sys.stderr,
        )
        return 3

    if args.json:
        payload = {
            "changed": total_changes,
            "questions": total_questions,
            "heuristic": total_heuristic,
            "score_drift": total_drift,
            "written": bool(args.write),
            "banks": [
                {
                    "label": bank.label,
                    "path": str(bank.path),
                    "questions": len(bank.plans),
                    "current": {
                        str(k): v for k, v in Counter(p.current for p in bank.plans).items()
                    },
                    "target": {str(k): v for k, v in Counter(p.target for p in bank.plans).items()},
                    "changes": [
                        {
                            "id": p.qid,
                            "type": p.qtype,
                            "from": p.current,
                            "to": p.target,
                            "k": p.k,
                            "a": p.a,
                            "c": p.c,
                            "s": round(p.s, 3),
                            "source": p.source,
                        }
                        for p in bank.changes
                    ],
                }
                for bank in plans
            ],
            "skipped": skipped,
        }
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        for bank in plans:
            report_bank(
                bank, max(1, args.max_report), args.big_mode, args.big_difficulty, args.big_score
            )
        if skipped:
            print("跳过的文件：")
            for line in skipped:
                print(f"  · {line}")
            print()
        print(
            f"题库 {len(plans)} 个，题 {total_questions} 道；"
            f"需改 {total_changes} 题；启发式估算 {total_heuristic} 题；"
            f"大题分值偏离统一口径 {total_drift} 题"
        )
        print("已写入" if args.write else "dry-run（未写入；加 --write 落盘）")

    if args.write:
        stamp = time.strftime("%Y%m%d-%H%M%S")
        for bank in plans:
            payload = load_json(bank.path)
            questions = payload.get("questions")
            if not isinstance(questions, list):
                continue
            missing_meta = 0
            for q in questions:
                meta = q.get("meta") if isinstance(q, dict) else None
                scoring = meta.get("scoring") if isinstance(meta, dict) else None
                if not (
                    isinstance(scoring, dict)
                    and all(is_int(scoring.get(key)) for key in ("k", "a", "c"))
                ):
                    missing_meta += 1
            if not bank.changes and (args.no_meta or not missing_meta):
                continue
            by_id = {plan.qid: plan for plan in bank.plans}
            for index, q in enumerate(questions):
                if not isinstance(q, dict):
                    continue
                qid = str(q.get("id") or f"#{index + 1}")
                plan = by_id.get(qid)
                if plan is None:
                    continue
                apply_plan(
                    q,
                    plan,
                    big_mode=args.big_mode,
                    big_score=args.big_score,
                    write_meta=not args.no_meta,
                    fix_score=args.fix_score,
                )
            if args.backup:
                backup = bank.path.with_name(f"{bank.path.name}.bak-{stamp}")
                backup.write_text(bank.path.read_text(encoding="utf-8"), encoding="utf-8")
            write_json(bank.path, payload)

    if args.strict and (total_changes or total_drift):
        return 1
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        print("已中断", file=sys.stderr)
        sys.exit(130)
