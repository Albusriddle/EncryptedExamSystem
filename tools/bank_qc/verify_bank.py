#!/usr/bin/env python3
"""题库门禁：只读校验 JSON 题库，**不修改任何题库文件**。

校验范围
--------
1. 顶层结构：``exam_id`` / ``title`` / ``pick`` / ``questions``；
2. 逐题字段：``id`` / ``type`` / ``stem`` / ``body`` / ``answer`` / ``judge`` /
   ``kp`` / ``difficulty`` / ``score``；
3. 答案自洽：选项键、填空占位符（个数 / 编号 / 顺序）、改错行号与原文、
   大题评分点合计；
4. 代码题语言：``code_language`` 与 ``code_languages`` 二选一，语言名白名单，
   大题必须写 ``code_languages: ["c"]``（可用 ``--big-lang`` 改）；
5. 题数（``--expect``）、难度取值、fill/fix 每档下限、选项与答案的分布；
6. 重复：库内逐字题干、骨架族上限、骨架种类下限、跨库逐字题干；
7. 原子化：题干不得引用其它题（上题 / 第 N 题 / 见图 …）；
8. 题干代码排版：程序被压成单行、用行内代码写整段程序、围栏未闭合、TAB 缩进；
9. 判题材料：``judge.tests >= 3``、官方答案回放全过、改错原始代码必失败、
   撤销任一修改后必失败（需要本地 C 编译器；``--no-compile`` 可跳过）；
10. ``meta.scoring`` 与 ``difficulty`` 的一致性（S 用 **S×100 整数**比较，见下）。

成对的旧格式（``format_version: 2`` 的 ``X.questions.json`` + ``X.answers.json``）会先在
内存里转换成原生形状，再走同一套检查；``difficulty=0`` 按"旧约定=多选"识别并提示迁移到 9。

设计约束
--------
* 只用 Python 标准库，不依赖 pydantic / PySide6，可在任何机器上直接跑；
* **不写死任何绝对路径**：被检目录、编译器位置、临时目录都由参数或环境决定；
* 编译器按 ``--clang`` → ``$CLANG`` → ``$CC`` → PATH 里的 ``clang`` / ``gcc`` / ``cc``
  依次查找；找不到、或找到了但连一个最小程序都编译不了时，**只报警告**并跳过回放
  （题库没有问题，是本机工具链的问题），加 ``--require-compile`` 才当作 error；
* 分数比较一律用整数百分位：``S×100 = 35K + 45A + 20C``，阈值也乘 100 比。
  浮点写法在 2.55 / 3.30 这类边界上会被二进制表示带偏（同分不同档），整数不吃这个亏；
* 只读：编译产物写在系统临时目录，退出前删除；
* 退出码：``0`` 无 error（warning 不算失败）；``1`` 有 error；``2`` 用法 / IO 问题。

用法见同目录 README.md，或 ``python verify_bank.py --help``。
"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from collections import Counter
from collections.abc import Iterable, Iterator
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

# ---------------------------------------------------------------- 常量

ERROR = "ERROR"
WARN = "WARN"

ALLOWED_TYPES = ("single", "multiple", "judge", "fill", "fix", "essay", "code")
TYPE_ALIASES = {"multi": "multiple", "blank": "fill", "blanks": "fill", "fix_code": "fix"}

#: 与 admin/app/core/models.py 的 allowed_langs 保持一致（11 种）
ALLOWED_LANGS = {
    "python",
    "c",
    "cpp",
    "java",
    "rust",
    "javascript",
    "typescript",
    "go",
    "sql",
    "bash",
    "text",
}

#: 围栏语言标记（不在表里的按"没写标记"处理，避免把 ```int 误认成语言）
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

WIN_RESERVED = {
    "con",
    "prn",
    "aux",
    "nul",
    *(f"com{i}" for i in range(1, 10)),
    *(f"lpt{i}" for i in range(1, 10)),
}
FORBIDDEN_CHARS = set('/\\:*?"<>|')

BLANK_RE = re.compile(r"____([①②③④⑤⑥⑦⑧⑨⑩])____")
BLANK_CHARS = "①②③④⑤⑥⑦⑧⑨⑩"

#: 围栏代码块：```lang\n…```；语言标记只吃同一行的非反引号字符
FENCE_RE = re.compile(r"```([^\n`]*)\n?(.*?)```", re.S)
INLINE_RE = re.compile(r"`([^`\n]*)`")

#: 跨题引用（只判题干，判题材料里允许出现"第 N 行"）
CROSS_REF_RE = re.compile(
    r"上题|上一题|上述|前述|同上|沿用|见图|见表|此题|本图|本表|"
    r"第\s*[0-9零一二三四五六七八九十百]+\s*题"
)

KP_RE = re.compile(r"(?:[1-9]|1[0-2])\.[1-9]")

#: 判题材料回放前缀（fill 的模板与 fix 的逐行代码都按 C 编译）
PRE = "#include <stdio.h>\n#include <string.h>\n#include <stdlib.h>\n"

DIFF_NEAR_LEVELS = (1, 2, 3)
MULTIPLE_DIFFICULTY = 9

#: 分档阈值，**S×100 的整数**（与文档里的 1.90 / 2.55 / 1.75 / 2.60 / 3.30 一一对应）
NEAR_CUTS = (190, 255)
BIG_CUTS = (175, 260, 330)
#: 多选真实 S 的允许区间（×100）
MULTI_S_RANGE = (200, 220)

#: 单选答案字母占比的门禁区间（只对 ≥40 道单选的库生效）
#: 出题目标是 450 题里每项 100–125（22%–28%），门禁留出加题余量后按 18%–32% 卡
LETTER_SHARE_RANGE = (0.18, 0.32)
LETTER_MIN_SAMPLE = 40
#: 与 "ABCD"[i%4] 的吻合率上限
CYCLE_SHARE_MAX = 0.35

#: 缺 meta.scoring 时最多逐题报几条，其余汇总成一条（避免 500 条刷屏）
META_WARN_LIMIT = 10


# ---------------------------------------------------------------- 数据结构


@dataclass
class Finding:
    """一条校验结论。"""

    severity: str
    scope: str
    where: str
    rule: str
    message: str

    def render(self) -> str:
        return f"{self.severity:<5} [{self.scope}] {self.where} [{self.rule}] {self.message}"


@dataclass
class Report:
    items: list[Finding] = field(default_factory=list)

    def add(self, severity: str, scope: str, where: str, rule: str, message: str) -> None:
        self.items.append(Finding(severity, scope, where, rule, message))

    def error(self, scope: str, where: str, rule: str, message: str) -> None:
        self.add(ERROR, scope, where, rule, message)

    def warn(self, scope: str, where: str, rule: str, message: str) -> None:
        self.add(WARN, scope, where, rule, message)

    @property
    def errors(self) -> list[Finding]:
        return [f for f in self.items if f.severity == ERROR]

    @property
    def warnings(self) -> list[Finding]:
        return [f for f in self.items if f.severity == WARN]


@dataclass
class Bank:
    """一个待检题库（原生格式，或已从旧格式在内存里转换出来的视图）。"""

    label: str
    path: Path
    data: dict
    questions: list[dict]
    legacy: bool = False
    total_questions: int = 0
    notes: list[str] = field(default_factory=list)
    #: 缺 meta.scoring 的题号：逐条报前 META_WARN_LIMIT 条，其余汇总（500 题刷屏没人看）
    meta_missing: list[str] = field(default_factory=list)


@dataclass
class Options:
    min_per_level: int = 15
    max_skeleton_family: int = 4
    big_difficulty: int = 3
    big_score: int = 15
    big_lang: str = "c"
    limit: int = 0


class UsageError(Exception):
    """参数 / IO 层面的问题（退出码 2），不是题库本身的问题。"""


class QCError(Exception):
    """旧格式转换失败。"""


# ---------------------------------------------------------------- 小工具


def force_utf8_when_piped() -> None:
    """重定向到文件/管道时按 UTF-8 输出。

    中文 Windows 的默认编码是 cp936：控制台里显示正常，但 `> report.txt` 出来的
    是 GBK，别的工具（含本仓库的文档流水线）按 UTF-8 读就会乱码。控制台不是
    管道时保持原样，避免反过来把 cp936 控制台写花。
    """
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


def iter_fences(stem: str) -> Iterator[tuple[str, str]]:
    """遍历题干里的围栏块，产出 (语言, 正文)。"""
    for match in FENCE_RE.finditer(stem or ""):
        lang, body = match.group(1).strip(), match.group(2)
        low = lang.lower()
        if low not in FENCE_LANGS:
            # ```int main(void){…}``` 这种没写语言标记的块：把标记并回正文
            body, lang, low = lang + body, "", ""
        yield low, body


def strip_fences(stem: str) -> str:
    return FENCE_RE.sub("", stem or "")


def normalize_stem(stem: str) -> str:
    """骨架：代码块 → <CODE>、行内 → <C>、数字 → <N>、去空白。"""
    text = FENCE_RE.sub("<CODE>", stem or "")
    text = re.sub(r"`[^`]*`", "<C>", text)
    text = re.sub(r"\d+", "<N>", text)
    return re.sub(r"\s+", "", text)


def compact_stem(stem: str) -> str:
    """逐字比较用：只去空白。"""
    return re.sub(r"\s+", "", stem or "")


def option_keys(q: dict) -> list[str]:
    body = q.get("body") if isinstance(q.get("body"), dict) else {}
    opts = body.get("options") or []
    return [str(o.get("key")) for o in opts if isinstance(o, dict)]


def option_texts(q: dict) -> list[str]:
    body = q.get("body") if isinstance(q.get("body"), dict) else {}
    opts = body.get("options") or []
    return [str(o.get("text")) for o in opts if isinstance(o, dict)]


def expected_output(case: dict) -> Any:
    """判题用例的期望输出：``expected`` 优先，兼容旧写法的 ``output``。"""
    if "expected" in case:
        return case.get("expected")
    return case.get("output")


# ---------------------------------------------------------------- clang


class Compiler:
    """C 编译器包装（clang / gcc 均可）：编译到临时目录，按用例喂 stdin。"""

    def __init__(self, exe: str | None, workdir: Path, timeout: int = 10) -> None:
        self.exe = exe
        self.name = Path(exe).name if exe else ""
        self.workdir = workdir
        self.timeout = timeout
        self._seq = 0

    @property
    def available(self) -> bool:
        return bool(self.exe)

    @staticmethod
    def discover(given: str | None) -> str | None:
        """按 --clang → $CLANG → $CC → PATH 里的 clang / gcc / cc 顺序找编译器。

        ``given`` 为 ``None``：走完整链条；为 ``""``：只表示"优先用 PATH 里的 clang"
        （``--clang`` 不带值时的语义）；为路径/名字：只用它，找不到就返回 None
        （不静默换成别的编译器，否则"我明明指定了 clang"会变成假象）。

        不写死任何安装路径：Windows 上 clang/gcc 装在哪由用户决定，把某台机器上的
        安装目录写进代码，换台机器就跑不了，还会把开发机的目录结构带进交付物。
        """
        if given is not None and given.strip():
            path = Path(given.strip()).expanduser()
            if path.is_file():
                return str(path)
            resolved = shutil.which(given.strip())
            return resolved

        # 不带值的 --clang：先把 PATH 里的 clang 排到最前（等价于"我就要 clang"），
        # 其余顺序不变；没这个开关时先看 $CLANG / $CC，尊重用户显式设置的环境变量。
        prefer_clang = given is not None
        candidates: list[str] = []
        if prefer_clang:
            found = shutil.which("clang")
            if found:
                candidates.append(found)
        for env_name in ("CLANG", "CC"):
            env = os.environ.get(env_name, "").strip()
            if env:
                candidates.append(env)
        for name in ("clang", "gcc", "cc"):
            found = shutil.which(name)
            if found:
                candidates.append(found)
        for cand in candidates:
            path = Path(cand)
            if path.is_file():
                return str(path)
            resolved = shutil.which(cand)
            if resolved:
                return resolved
        return None

    def _tag(self, tag: str) -> str:
        self._seq += 1
        safe = re.sub(r"[^A-Za-z0-9_.-]", "_", tag)[:48] or "q"
        return f"{safe}_{self._seq:05d}"

    def compile(self, code: str, tag: str) -> tuple[str | None, str | None]:
        """编译成可执行文件，返回 (可执行文件路径, 错误说明)。"""
        if not self.exe:
            return None, "未找到 C 编译器"
        name = self._tag(tag)
        src = self.workdir / f"{name}.c"
        out = self.workdir / (f"{name}.exe" if os.name == "nt" else name)
        src.write_text(code, encoding="utf-8")
        try:
            proc = subprocess.run(
                [self.exe, "-O0", "-std=c11", "-w", "-o", str(out), str(src)],
                capture_output=True,
                text=True,
                encoding="utf-8",
                errors="replace",
                timeout=max(self.timeout, 20),
            )
        except subprocess.TimeoutExpired:
            return None, "编译超时"
        except OSError as exc:
            return None, f"无法启动编译器（{self.name}）：{exc}"
        if proc.returncode != 0:
            text = ((proc.stderr or "") + "\n" + (proc.stdout or "")).strip()
            first = [line for line in text.splitlines() if line.strip()]
            detail = first[0] if first else (
                f"退出码 {proc.returncode}，且编译器没有任何输出"
                "（这个编译器可能在本机根本跑不起来，先用 --clang 换一个试试）"
            )
            return None, f"编译失败：{detail[:200]}"
        return str(out), None

    def run(self, exe: str, stdin: str) -> tuple[str | None, str | None]:
        try:
            proc = subprocess.run(
                [exe],
                input=stdin or "",
                capture_output=True,
                text=True,
                encoding="utf-8",
                errors="replace",
                timeout=self.timeout,
            )
        except subprocess.TimeoutExpired:
            return None, "运行超时"
        except OSError as exc:
            return None, f"无法运行：{exc}"
        return proc.stdout, None


PREFLIGHT_SRC = "int main(void){return 0;}\n"


def check_compiler(compiler: Compiler) -> str | None:
    """试编译一个最小程序；返回 None 表示编译器可用，否则返回原因。

    为什么值得多花一次编译：本机 gcc 装坏时（能启动、但 cc1 静默退出 1），
    逐题回放会产出 121 条"编译失败"，看上去像**题库全错**。先做一次冒烟，
    就能把"工具链坏了"和"题目坏了"分开——这两件事的处置完全不同。
    """
    _exe, err = compiler.compile(PREFLIGHT_SRC, "preflight")
    return err


def run_cases(compiler: Compiler, code: str, cases: list[dict], tag: str) -> str | None:
    """编译一次、逐用例比对。全部通过返回 None，否则返回第一条失败说明。"""
    exe, err = compiler.compile(code, tag)
    if err:
        return err
    assert exe is not None
    for i, case in enumerate(cases):
        got, run_err = compiler.run(exe, str(case.get("input") or ""))
        if run_err:
            return f"用例 {i + 1} {run_err}"
        want = str(expected_output(case) if expected_output(case) is not None else "")
        norm_got = (got or "").replace("\r\n", "\n").strip()
        norm_want = want.replace("\r\n", "\n").strip()
        if norm_got != norm_want:
            return (
                f"用例 {i + 1}（input={case.get('input')!r}）期望 {norm_want!r}，实际 {norm_got!r}"
            )
    return None


# ---------------------------------------------------------------- 加载


def base_name(path: Path) -> str:
    """``choice.questions(1).json`` → ``choice``。与 bank_import.base_name 一致。"""
    name = path.name
    if name.lower().endswith(".json"):
        name = name[: -len(".json")]
    name = re.sub(r"\s*\(\d+\)$", "", name)
    for suffix in (".questions", ".answers"):
        if name.lower().endswith(suffix):
            name = name[: -len(suffix)]
    return name


def detect(payload: Any) -> str:
    if not isinstance(payload, dict):
        return "unknown"
    if payload.get("format_version") == 2:
        role = payload.get("role")
        if role == "questions":
            return "legacy_questions"
        if role == "answers":
            return "legacy_answers"
        return "unknown"
    if "exam_id" in payload and "questions" in payload:
        return "native"
    return "unknown"


def load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8-sig"))
    except OSError as exc:
        raise UsageError(f"读不了 {path}：{exc}") from exc
    except UnicodeDecodeError as exc:
        raise UsageError(f"{path} 不是 UTF-8：{exc}（另存为 UTF-8 后重试）") from exc
    except json.JSONDecodeError as exc:
        raise UsageError(f"{path} 不是合法 JSON：第 {exc.lineno} 行 {exc.msg}") from exc


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
    seen: set[Path] = set()
    unique: list[Path] = []
    for path in files:
        key = path.resolve()
        if key in seen:
            continue
        seen.add(key)
        unique.append(path)
    return unique


def _check_count(name: str, declared: Any, actual: int) -> None:
    if declared is None:
        return
    if not is_int(declared):
        raise QCError(f"{name}: count 必须是整数，实际 {declared!r}")
    if int(declared) != actual:
        raise QCError(f"{name}: count={declared}，实际有 {actual} 条")


def convert_legacy_pair(q_payload: dict, a_payload: dict, qname: str, aname: str) -> dict:
    """把 format_version=2 的题目/答案两文件合成原生形状（只在内存里）。

    规则与 admin/app/core/bank_import.py 的 convert_pair 对齐；本函数只做
    "能读出来"这一件事，更细的质量检查交给后面的统一校验。
    """
    qtype = str(q_payload.get("type", ""))
    if qtype not in {"choice", "fill_blank", "fix_code"}:
        raise QCError(
            f"{qname}: 旧格式只支持 type=choice / fill_blank / fix_code，实际 {qtype!r}"
            "（代码简答题请用原生 JSON）"
        )
    if a_payload.get("type") != qtype:
        raise QCError(f"{aname}: type={a_payload.get('type')!r} 与题目文件的 {qtype!r} 不一致")
    qs = q_payload.get("questions")
    ans = a_payload.get("answers")
    if not isinstance(qs, list) or not isinstance(ans, list):
        raise QCError(f"{qname}: 缺少 questions/answers 数组")
    _check_count(qname, q_payload.get("count"), len(qs))
    _check_count(aname, a_payload.get("count"), len(ans))

    q_ids = [str(x.get("id")) for x in qs if isinstance(x, dict)]
    a_ids = [str(x.get("id")) for x in ans if isinstance(x, dict)]
    if len(set(q_ids)) != len(q_ids):
        raise QCError(f"{qname}: 题目 ID 重复")
    if len(set(a_ids)) != len(a_ids):
        raise QCError(f"{aname}: 答案 ID 重复")
    missing = sorted(set(q_ids) - set(a_ids))
    orphan = sorted(set(a_ids) - set(q_ids))
    if missing or orphan:
        parts = []
        if missing:
            parts.append(f"缺少答案：{', '.join(missing[:5])}")
        if orphan:
            parts.append(f"多出答案：{', '.join(orphan[:5])}")
        raise QCError(f"{aname}: " + "；".join(parts))

    by_id = {str(x.get("id")): x for x in ans if isinstance(x, dict)}
    out: list[dict] = []
    for idx, raw in enumerate(qs):
        if not isinstance(raw, dict):
            raise QCError(f"{qname} 第 {idx + 1} 条不是对象")
        qid = str(raw.get("id") or "").strip()
        where = f"{qname} 第 {idx + 1} 条（id={qid}）"
        answer = by_id.get(qid) or {}
        verdict = answer.get("verdict") or {}
        content = raw.get("content") or {}
        lang = content.get("kind")
        difficulty = raw.get("difficulty")
        if difficulty is not None and not is_int(difficulty):
            raise QCError(f"{where}: difficulty 必须是整数，实际 {difficulty!r}")

        legacy_multi = False
        if qtype == "choice":
            if lang != "options":
                raise QCError(f"{where}: 选择题 content.kind 应为 options，实际 {lang!r}")
            options = content.get("options")
            if not isinstance(options, dict) or not options:
                raise QCError(f"{where}: content.options 必须是非空对象")
            keys = list(options.keys())
            want = [chr(ord("A") + i) for i in range(len(keys))]
            if keys != want:
                raise QCError(f"{where}: 选项键应为 {want}，实际 {keys}")
            body = {
                "kind": "options",
                "options": [{"key": k, "text": str(v)} for k, v in options.items()],
            }
            internal = "multiple" if difficulty == 0 else "single"
            legacy_multi = difficulty == 0
            if legacy_multi != (verdict.get("kind") == "multi"):
                raise QCError(
                    f"{where}: difficulty={difficulty}"
                f"（{'多选' if legacy_multi else '单选'}）与 verdict.kind="
                    f"{verdict.get('kind')!r} 矛盾"
                )
        elif qtype == "fill_blank":
            if lang != "template":
                raise QCError(f"{where}: 补全题 content.kind 应为 template，实际 {lang!r}")
            body = {
                "kind": "template",
                "template": str(content.get("template") or ""),
                "blank_count": content.get("blank_count"),
            }
            internal = "fill"
        else:
            if lang != "code":
                raise QCError(f"{where}: 改错题 content.kind 应为 code，实际 {lang!r}")
            code = content.get("code")
            if not isinstance(code, list) or not code:
                raise QCError(f"{where}: content.code 必须是非空数组")
            body = {
                "kind": "code",
                "code": [str(x) for x in code],
                "blank_count": content.get("blank_count"),
            }
            internal = "fix"

        question = {
            "id": qid,
            "type": internal,
            "stem": str(raw.get("stem") or ""),
            "body": body,
            "answer": verdict,
            "judge": answer.get("judge"),
            "kp": raw.get("kp"),
            "difficulty": difficulty,
            "score": 2,  # 旧格式没有 score，bank_import 也按 2 处理
            "_legacy": True,
            "_legacy_multi": legacy_multi,
        }
        out.append(question)
    return {
        "exam_id": Path(qname).name,
        "title": Path(qname).name,
        "pick": len(out),
        "questions": out,
        "_legacy": True,
    }


def find_legacy_answers(path: Path) -> Path | None:
    stem = base_name(path)
    for pattern in (f"{stem}.answers.json", f"{stem}.answers*.json"):
        for cand in sorted(path.parent.glob(pattern)):
            if cand == path:
                continue
            try:
                payload = load_json(cand)
            except UsageError:
                continue
            if detect(payload) == "legacy_answers":
                return cand
    return None


def load_banks(files: list[Path], report: Report, opts: Options) -> tuple[list[Bank], list[str]]:
    banks: list[Bank] = []
    notes: list[str] = []
    broken: dict[Path, str] = {}

    # 先认一遍"被成对引用的答案文件"：目录里同时有 X.questions.json 与
    # X.answers.json 时，答案文件是配对材料，不该再当成一个独立题库报错。
    paired_answers: set[Path] = set()
    for path in files:
        try:
            payload = load_json(path)
        except UsageError as exc:
            # 单个文件坏了不该让整轮门禁变成"用法错误"（退出码 2）：
            # 那是**这份题库**的问题，记成 ERROR、其余题库继续查（退出码 1）。
            broken[path] = str(exc)
            continue
        if detect(payload) == "legacy_questions":
            sibling = find_legacy_answers(path)
            if sibling is not None:
                paired_answers.add(sibling)

    for path, message in broken.items():
        report.error(path.stem, "顶层", "读取", message)

    consumed: set[Path] = set(paired_answers)
    for path in files:
        if path in consumed or path in broken:
            continue
        try:
            payload = load_json(path)
        except UsageError as exc:
            report.error(path.stem, "顶层", "读取", str(exc))
            continue
        fmt = detect(payload)
        label = path.stem
        if fmt == "native":
            questions = payload.get("questions")
            if not isinstance(questions, list):
                report.error(label, "顶层", "结构", "questions 必须是数组")
                continue
            bank = Bank(
                label=label,
                path=path,
                data=payload,
                questions=list(questions),
                total_questions=len(questions),
                notes=[f"原生格式：{path.name}"],
            )
            banks.append(bank)
        elif fmt == "legacy_questions":
            answers = find_legacy_answers(path)
            if answers is None:
                report.error(
                    label,
                    "顶层",
                    "旧格式",
                    f"找不到同名答案文件（{base_name(path)}.answers.json）——旧格式必须成对提供",
                )
                continue
            try:
                data = convert_legacy_pair(
                    payload, load_json(answers), qname=path.name, aname=answers.name
                )
            except QCError as exc:
                report.error(label, "顶层", "旧格式", str(exc))
                continue
            consumed.add(answers)
            bank = Bank(
                label=label,
                path=path,
                data=data,
                questions=list(data["questions"]),
                legacy=True,
                total_questions=len(data["questions"]),
                notes=[
                    f"旧格式（format_version=2）：{path.name} + {answers.name}",
                    "旧格式无 score（按 2 分计）、无 kp 校验降级为 WARN",
                ],
            )
            banks.append(bank)
            report.warn(
                label,
                "顶层",
                "旧格式",
                "本次是在内存里转换后校验的旧格式；迁移到原生 JSON 时多选题须显式写 difficulty=9",
            )
        elif fmt == "legacy_answers":
            report.error(
                label,
                "顶层",
                "旧格式",
                "这是答案文件（role=answers），请把同名的 .questions.json 一起传入",
            )
        else:
            report.error(
                label,
                "顶层",
                "结构",
                "无法识别的格式：既没有 exam_id/questions，也不是 format_version=2 的题目文件",
            )
    if opts.limit > 0:
        for bank in banks:
            if len(bank.questions) > opts.limit:
                bank.questions = bank.questions[: opts.limit]
                notes.append(f"[{bank.label}] --limit {opts.limit}：只校验了前 {opts.limit} 题")
    return banks, notes


# ---------------------------------------------------------------- 逐题校验


def check_exam_id(value: str) -> str | None:
    """返回问题说明；没问题返回 None。规则与 models.validate_exam_id 一致。"""
    if not value:
        return "不能为空"
    if len(value) > 64:
        return f"过长（{len(value)} 字符，最多 64）"
    if any(c in FORBIDDEN_CHARS for c in value):
        return f"不能包含路径分隔符或 Windows 禁用字符（当前 {value!r}）"
    if any(ord(c) < 0x20 or c.isspace() for c in value):
        return f"不能包含空白或控制字符（当前 {value!r}）"
    if value.startswith(".") or value.endswith("."):
        return f"不能以 . 开头或结尾（当前 {value!r}）"
    if value.lower() in WIN_RESERVED:
        return f"不能使用 Windows 保留名（当前 {value!r}）"
    return None


def check_top(bank: Bank, report: Report, opts: Options) -> None:
    data = bank.data
    label = bank.label
    total = len(bank.questions)
    exam_id = data.get("exam_id")
    if not isinstance(exam_id, str) or not exam_id.strip():
        report.error(label, "顶层", "结构", "缺少 exam_id（字符串，必填）")
    else:
        problem = check_exam_id(exam_id)
        if problem:
            report.error(label, "顶层", "结构", f"exam_id {problem}")
    title = data.get("title")
    if not isinstance(title, str) or not title.strip():
        report.error(label, "顶层", "结构", "缺少 title（字符串，必填）")
    elif not opts.limit:
        # title 是老师在建场次时唯一看到的名字，里面写"440 分"而实际 450 会直接
        # 误导人（本库两套大题标题就是这么写错的）。系统不算 title，所以只提醒。
        scores = [int(x) for x in re.findall(r"(\d+)\s*分", title)]
        if len(scores) == 1:
            real = sum(q.get("score") for q in bank.questions if is_int(q.get("score")))
            if real and scores[0] != real:
                report.warn(
                    label,
                    "顶层",
                    "结构",
                    f"title 里写「{scores[0]} 分」，但本题库分值和是 {real} 分："
                    "系统不读 title，改一下免得误导",
                )
        counts = [int(x) for x in re.findall(r"(\d+)\s*题", title)]
        if len(counts) == 1 and counts[0] != total:
            report.warn(
                label,
                "顶层",
                "结构",
                f"title 里写「{counts[0]} 题」，实际 {total} 题",
            )
    pick = data.get("pick")
    if not is_int(pick):
        report.error(label, "顶层", "结构", f"pick 必须是整数，实际 {pick!r}")
    elif pick < 0:
        report.error(label, "顶层", "结构", f"pick={pick} 不能为负数")
    elif opts.limit:
        pass  # --limit 是冒烟开关，pick / 题数 / 分布都失去意义，不判
    elif pick > total:
        report.error(label, "顶层", "结构", f"pick={pick} 超过题量 {total}（导入会被拒绝）")
    elif pick == 0:
        report.warn(label, "顶层", "结构", "pick=0 表示空卷，打包前请确认这是本意")
    if total == 0:
        report.error(label, "顶层", "结构", "questions 为空")

    ids = [str(q.get("id")) for q in bank.questions if isinstance(q, dict)]
    dup = [qid for qid, n in Counter(ids).items() if n > 1]
    if dup:
        report.error(
            label,
            "顶层",
            "重复 id",
            f"题目 ID 重复：{', '.join(sorted(dup)[:5])}——改 id 等于换了一份卷子",
        )
    for idx, q in enumerate(bank.questions):
        if not isinstance(q, dict):
            report.error(label, f"#{idx + 1}", "结构", "题目不是对象")


def check_meta_scoring(q: dict, where: str, report: Report, bank: Bank, qtype: str) -> None:
    meta = q.get("meta")
    if meta is None:
        bank.meta_missing.append(str(q.get("id") or where))
        if len(bank.meta_missing) <= META_WARN_LIMIT:
            report.warn(
                bank.label,
                where,
                "难度",
                "没有 meta.scoring（K/A/C）：score_bank.py 只能给启发式估算，建议补标注",
            )
        return
    if not isinstance(meta, dict):
        report.warn(bank.label, where, "难度", "meta 不是对象，已忽略")
        return
    scoring = meta.get("scoring")
    if scoring is None:
        report.warn(bank.label, where, "难度", "meta 里没有 scoring（k/a/c）")
        return
    if not isinstance(scoring, dict):
        report.error(bank.label, where, "难度", "meta.scoring 必须是对象 {k,a,c}")
        return
    values = {key: scoring.get(key) for key in ("k", "a", "c")}
    if not all(is_int(v) for v in values.values()):
        report.error(bank.label, where, "难度", f"meta.scoring 的 k/a/c 必须是整数，实际 {values}")
        return
    k, a, c = int(values["k"]), int(values["a"]), int(values["c"])
    if not (1 <= k <= 3) or not (1 <= a <= 4) or not (1 <= c <= 4):
        report.error(
            bank.label,
            where,
            "难度",
            f"meta.scoring 越界（K 1–3 / A 1–4 / C 1–4），实际 k={k} a={a} c={c}",
        )
        return
    s100 = 35 * k + 45 * a + 20 * c  # S×100，整数比较
    declared = q.get("difficulty")
    if qtype == "multiple":
        low, high = MULTI_S_RANGE
        if not (low <= s100 <= high):
            report.error(
                bank.label,
                where,
                "难度",
                f"多选题真实 S={s100 / 100:.2f} 不在 [{low / 100:.2f}, {high / 100:.2f}]"
                f"（k={k} a={a} c={c}）——要么统一结构，要么重标 K/A/C",
            )
        return
    if not is_int(declared):
        return
    if qtype == "code":
        cut1, cut2, cut3 = BIG_CUTS
        want = 1 if s100 < cut1 else 2 if s100 < cut2 else 3 if s100 < cut3 else 4
    else:
        cut1, cut2 = NEAR_CUTS
        want = 1 if s100 < cut1 else 2 if s100 < cut2 else 3
    if want != declared:
        report.error(
            bank.label,
            where,
            "难度",
            f"K/A/C 算得 S={s100 / 100:.2f} 应属难度 {want}，实际写 {declared}",
        )


def check_code_layout(q: dict, where: str, report: Report, bank: Bank) -> None:
    stem = q.get("stem") or ""
    if stem.count("```") % 2:
        # 未闭合的围栏会被 Markdown 当普通文本整段显示，代码块样式全丢；
        # 学生看到的是反引号原文，考场没法读题。
        report.error(
            bank.label,
            where,
            "代码排版",
            "围栏代码块未闭合（``` 必须成对出现）",
        )
    code_lines: list[str] = []
    fences: list[list[str]] = []
    for _lang, body in iter_fences(stem):
        text = body.strip("\n")
        lines = [line for line in text.splitlines() if line.strip()]
        fences.append(lines)
        code_lines.extend(lines)
        if not text.strip():
            report.error(bank.label, where, "代码排版", "存在空代码块")
            continue
        if len(lines) != 1:
            continue
        one = lines[0].strip()
        if "int main" in one:
            report.error(
                bank.label,
                where,
                "代码排版",
                "完整程序被压成单行：必须换行、`{`/`}` 独占一行、4 空格缩进",
            )
        elif len(one) > 40 and ";" in one:
            report.error(
                bank.label,
                where,
                "代码排版",
                f"单行围栏块 {len(one)} 字符（上限 40）：要么拆成多行，要么改用行内代码",
            )
    body = q.get("body") if isinstance(q.get("body"), dict) else {}
    if isinstance(body.get("code"), list):
        code_lines.extend(str(x) for x in body["code"])
    for line in code_lines:
        head, _, tail = line.partition("int main")
        if tail and "{" in tail:
            report.error(
                bank.label,
                where,
                "代码排版",
                f"int main 与函数体写在同一行：`{{`/`}}` 必须独占一行（{line.strip()[:60]}）",
            )
            break
    for line in code_lines:
        if "\t" in line:
            report.warn(
                bank.label,
                where,
                "代码排版",
                "代码里出现 TAB 缩进：统一用 4 空格（TAB 在不同宽度下会错位）",
            )
            break
    inline_source = strip_fences(stem)
    for match in INLINE_RE.finditer(inline_source):
        code = match.group(1)
        if "int main" in code or (len(code) > 60 and ";" in code and "{" in code):
            report.error(
                bank.label,
                where,
                "代码排版",
                "程序片段用了行内代码：请改成多行围栏块（```c … ```）",
            )


def check_atomic(q: dict, where: str, report: Report, bank: Bank) -> None:
    stem = q.get("stem") or ""
    hit = CROSS_REF_RE.search(stem)
    if hit:
        report.error(
            bank.label,
            where,
            "原子化",
            f"题干含跨题引用「{hit.group(0)}」：每题必须自含，不得引用其它题",
        )


def check_language(
    q: dict, where: str, report: Report, bank: Bank, qtype: str, opts: Options
) -> None:
    single = q.get("code_language")
    multi = q.get("code_languages")
    multi_list = multi if isinstance(multi, list) else []
    if multi is not None and not isinstance(multi, list):
        report.error(bank.label, where, "代码语言", "code_languages 必须是数组")
        multi_list = []
    if isinstance(single, str) and single.strip() and multi_list:
        report.error(
            bank.label,
            where,
            "代码语言",
            "code_language 与 code_languages 不能同时填写，二者只写一个",
        )
    if isinstance(single, str) and single.strip():
        low = single.strip().lower()
        if low not in ALLOWED_LANGS:
            report.error(
                bank.label,
                where,
                "代码语言",
                f"code_language={single!r} 不在白名单（{', '.join(sorted(ALLOWED_LANGS))}）",
            )
    cleaned: list[str] = []
    for raw in multi_list:
        low = str(raw).strip().lower()
        if low not in ALLOWED_LANGS:
            report.error(
                bank.label,
                where,
                "代码语言",
                f"code_languages 里的 {raw!r} 不在白名单（{', '.join(sorted(ALLOWED_LANGS))}）",
            )
            continue
        cleaned.append(low)
    if len(multi_list) > 8:
        report.error(
            bank.label, where, "代码语言", f"code_languages 有 {len(multi_list)} 个，上限 8"
        )
    if len(set(cleaned)) != len(cleaned):
        report.warn(bank.label, where, "代码语言", "code_languages 有重复项，导入时会被去重")
    if qtype != "code" and (cleaned or (isinstance(single, str) and single.strip())):
        report.warn(bank.label, where, "代码语言", "非 code 题型不应声明代码语言，打包无害但请删掉")
    if qtype == "code" and opts.big_lang:
        want = opts.big_lang.strip().lower()
        if cleaned != [want]:
            report.error(
                bank.label,
                where,
                "代码语言",
                f'大题必须写 code_languages: ["{want}"]，实际 '
                f"{multi_list if multi_list else single!r}",
            )


def check_judge_decl(q: dict, where: str, report: Report, bank: Bank, qtype: str) -> bool:
    """结构层检查；返回能否进入编译回放。"""
    judge = q.get("judge")
    if qtype not in ("fill", "fix"):
        if judge is not None:
            report.warn(bank.label, where, "判题", "只有 fill/fix 需要 judge；该字段打包时会被剥离")
        return False
    if not isinstance(judge, dict):
        report.error(
            bank.label,
            where,
            "判题",
            "缺少 judge（fill/fix 必须可编译回放：entry/harness/tests）",
        )
        return False
    ok = True
    entry = judge.get("entry")
    harness = judge.get("harness")
    tests = judge.get("tests")
    if not isinstance(entry, str) or not entry.strip():
        report.error(bank.label, where, "判题", "judge.entry 缺失（被调用的函数名）")
    if not isinstance(harness, str) or not harness.strip():
        report.error(bank.label, where, "判题", "judge.harness 缺失（带 main 的驱动程序）")
        ok = False
    elif isinstance(entry, str) and entry.strip() and entry.strip() not in harness:
        report.warn(
            bank.label,
            where,
            "判题",
            f"judge.entry={entry!r} 没出现在 harness 里，确认 harness 真的调用了它",
        )
    if not isinstance(tests, list):
        report.error(bank.label, where, "判题", "judge.tests 必须是数组")
        return False
    if len(tests) < 3:
        report.error(
            bank.label,
            where,
            "判题",
            f"judge.tests 只有 {len(tests)} 条（下限 3，且至少 1 条边界）",
        )
        ok = False
    for i, case in enumerate(tests):
        if not isinstance(case, dict):
            report.error(bank.label, where, "判题", f"第 {i + 1} 条用例不是对象")
            ok = False
            continue
        if "input" not in case:
            report.error(bank.label, where, "判题", f"第 {i + 1} 条用例缺少 input")
            ok = False
        if expected_output(case) is None:
            report.error(
                bank.label,
                where,
                "判题",
                f"第 {i + 1} 条用例缺少 expected（兼容旧写法的 output）",
            )
            ok = False
    if ok and not any(
        str(case.get("note") or "").strip() for case in tests if isinstance(case, dict)
    ):
        report.warn(
            bank.label,
            where,
            "判题",
            "所有用例都没写 note：边界用例建议在 note 里注明（本工具查不出覆盖是否真的够）",
        )
    if isinstance(harness, str) and re.search(r"fopen|fprintf|freopen", harness):
        report.warn(
            bank.label,
            where,
            "判题",
            "harness 里有文件读写：回放时写当前工作目录，并发下会互相覆盖",
        )
    return ok


def check_answer(q: dict, where: str, report: Report, bank: Bank, qtype: str) -> None:
    answer = q.get("answer")
    keys = option_keys(q)
    if qtype in ("single", "judge"):
        if not isinstance(answer, dict):
            report.error(
                bank.label,
                where,
                "答案",
                'answer 必须是对象 {"kind":"single","answer":"B"}；写纯字母会导致判分取到空答案',
            )
            return
        if answer.get("kind") != "single":
            report.error(
                bank.label, where, "答案", f"answer.kind 应为 single，实际 {answer.get('kind')!r}"
            )
        value = answer.get("answer")
        if not isinstance(value, str) or not value:
            report.error(
                bank.label, where, "答案", f"answer.answer 必须是非空字符串，实际 {value!r}"
            )
        elif keys and value not in keys:
            report.error(
                bank.label,
                where,
                "答案",
                f"答案 {value!r} 不在选项 {keys} 中——学生永远选不到正确项",
            )
    elif qtype == "multiple":
        if not isinstance(answer, dict) or answer.get("kind") != "multi":
            report.error(
                bank.label,
                where,
                "答案",
                "多选 answer.kind 必须 multi，answer 为字母数组",
            )
            return
        value = answer.get("answer")
        if not isinstance(value, list) or not value:
            report.error(bank.label, where, "答案", "多选 answer 必须是非空数组")
            return
        letters = [str(x) for x in value]
        if len(letters) < 2:
            report.error(bank.label, where, "答案", "多选至少 2 个正确项（单正确项请用 single）")
        if len(letters) > 3:
            report.error(
                bank.label,
                where,
                "答案",
                f"多选正确项 2–3 个，实际 {len(letters)} 个（全对属于凑数题）",
            )
        if len(set(letters)) != len(letters):
            report.error(bank.label, where, "答案", f"多选答案有重复字母 {letters}")
        if letters != sorted(letters):
            report.error(bank.label, where, "答案", f"多选答案未按字母升序：{letters}")
        bad = [x for x in letters if keys and x not in keys]
        if bad:
            report.error(bank.label, where, "答案", f"多选答案 {bad} 不在选项 {keys} 中")
        elif keys and set(letters) == set(keys):
            report.error(
                bank.label,
                where,
                "答案",
                "多选答案等于全部选项（送分题）：正确项必须 2–3 个，不能四选四",
            )
    elif qtype == "fill":
        body = q.get("body") if isinstance(q.get("body"), dict) else {}
        if not isinstance(answer, dict) or answer.get("kind") != "blanks":
            report.error(bank.label, where, "答案", "填空 answer.kind 必须 blanks")
            return
        blank_count = body.get("blank_count")
        template = body.get("template") or ""
        marks = BLANK_RE.findall(template)
        if not is_int(blank_count) or blank_count < 1:
            report.error(
                bank.label, where, "答案", f"blank_count 必须是 ≥1 的整数，实际 {blank_count!r}"
            )
            blank_count = len(marks)
        if len(marks) != blank_count:
            report.error(
                bank.label,
                where,
                "答案",
                f"模板里有 {len(marks)} 个 ____①____ 占位符，但 blank_count={blank_count}",
            )
        if len(set(marks)) != len(marks):
            report.error(bank.label, where, "答案", f"占位符编号重复：{marks}")
        elif sorted(marks) == list(BLANK_CHARS[: len(marks)]) and marks != list(
            BLANK_CHARS[: len(marks)]
        ):
            # 判分按"第 k 个空 ↔ answer[k]"逐位比对，模板里 ③ 出现在 ① 前面时，
            # 学生按出现顺序填就全错位。规格要求 ①②③ 依次出现，这里只提醒不拦。
            report.warn(
                bank.label,
                where,
                "答案",
                f"占位符出现顺序是 {marks}，建议按 {'①②③'[: len(marks)]} 顺序编号"
                "（判分按编号对位，乱序容易被学生按位置填错）",
            )
        items = answer.get("answer")
        if not isinstance(items, list):
            report.error(bank.label, where, "答案", "填空 answer.answer 必须是数组")
            return
        if len(items) != blank_count:
            report.error(
                bank.label,
                where,
                "答案",
                f"有 {len(items)} 个空答案，但 blank_count={blank_count}",
            )
        for i, item in enumerate(items):
            if not isinstance(item, dict):
                report.error(bank.label, where, "答案", f"第 {i + 1} 个空答案不是对象")
                continue
            text = item.get("answer")
            if not isinstance(text, str) or not text.strip():
                report.error(bank.label, where, "答案", f"第 {i + 1} 个空答案是空串")
            no = item.get("no")
            if no is not None and i < len(BLANK_CHARS) and str(no) != BLANK_CHARS[i]:
                report.warn(
                    bank.label,
                    where,
                    "答案",
                    f"第 {i + 1} 个空的 no={no!r}，与占位符 {BLANK_CHARS[i]} 对不上"
                    "（no 只用于展示，但会误导阅卷）",
                )
        if blank_count != 3:
            report.warn(
                bank.label, where, "答案", f"本项目 fill 统一 3 空，当前 blank_count={blank_count}"
            )
    elif qtype == "fix":
        body = q.get("body") if isinstance(q.get("body"), dict) else {}
        code = body.get("code") if isinstance(body.get("code"), list) else []
        blank_count = body.get("blank_count")
        if not isinstance(answer, dict) or answer.get("kind") != "fixes":
            report.error(bank.label, where, "答案", "改错 answer.kind 必须 fixes")
            return
        fixes = answer.get("answer")
        if not isinstance(fixes, list):
            report.error(bank.label, where, "答案", "改错 answer.answer 必须是数组")
            return
        if not is_int(blank_count) or blank_count < 1:
            report.error(
                bank.label, where, "答案", f"blank_count 必须是 ≥1 的整数，实际 {blank_count!r}"
            )
            blank_count = len(fixes)
        if len(fixes) != blank_count:
            report.error(
                bank.label,
                where,
                "答案",
                f"有 {len(fixes)} 条改正项，但 blank_count={blank_count}",
            )
        used_lines: list[int] = []
        for i, fix in enumerate(fixes):
            if not isinstance(fix, dict):
                report.error(bank.label, where, "答案", f"第 {i + 1} 条改正项不是对象")
                continue
            line = fix.get("line")
            if not is_int(line) or not (1 <= line <= len(code)):
                report.error(
                    bank.label,
                    where,
                    "答案",
                    f"第 {i + 1} 条改正项的 line={line!r} 超出 1..{len(code)}",
                )
                continue
            used_lines.append(line)
            if fix.get("wrong") != code[line - 1]:
                report.error(
                    bank.label,
                    where,
                    "答案",
                    f"第 {i + 1} 条改正项的 wrong 与第 {line} 行原文不一致\n"
                    f"      原文: {code[line - 1]!r}\n      wrong: {fix.get('wrong')!r}",
                )
            text = fix.get("answer")
            if not isinstance(text, str) or not text.strip():
                report.error(bank.label, where, "答案", f"第 {i + 1} 条改正项缺少修正后的整行")
            elif text == fix.get("wrong"):
                report.error(
                    bank.label,
                    where,
                    "答案",
                    f"第 {i + 1} 条改正项改前改后完全相同（等于没改）",
                )
        if len(set(used_lines)) != len(used_lines):
            report.error(bank.label, where, "答案", f"多条改正项指向同一行：{used_lines}")
        if blank_count != 2:
            report.warn(
                bank.label, where, "答案", f"本项目 fix 统一 2 处错，当前 blank_count={blank_count}"
            )
    elif qtype == "code":
        if not isinstance(answer, dict):
            report.error(bank.label, where, "答案", "大题 answer 必须是对象（kind=rubric）")
            return
        if answer.get("kind") != "rubric":
            report.error(
                bank.label,
                where,
                "答案",
                f"大题 answer.kind 必须 rubric，实际 {answer.get('kind')!r}",
            )
        total = answer.get("total")
        score = q.get("score")
        if not is_int(total):
            report.error(bank.label, where, "答案", f"rubric.total 必须是整数，实际 {total!r}")
        elif is_int(score) and total != score:
            report.error(bank.label, where, "答案", f"rubric.total={total} 与 score={score} 不一致")
        points = answer.get("points")
        if not isinstance(points, list) or not points:
            report.error(bank.label, where, "答案", "rubric.points 必须是非空数组")
        else:
            subtotal = 0
            for i, point in enumerate(points):
                if not isinstance(point, dict):
                    report.error(bank.label, where, "答案", f"第 {i + 1} 个评分点不是对象")
                    continue
                if not str(point.get("text") or "").strip():
                    report.error(bank.label, where, "答案", f"第 {i + 1} 个评分点缺少 text")
                if not is_int(point.get("score")):
                    report.error(
                        bank.label, where, "答案", f"第 {i + 1} 个评分点的 score 必须是整数"
                    )
                else:
                    subtotal += int(point["score"])
            want = total if is_int(total) else score
            if is_int(want) and subtotal != want:
                report.error(
                    bank.label,
                    where,
                    "答案",
                    f"评分点合计 {subtotal} != 总分 {want}",
                )
        reference = answer.get("reference")
        if not isinstance(reference, str) or not reference.strip():
            report.error(bank.label, where, "答案", "rubric.reference 缺失（参考答案，仅教师端）")
        samples = answer.get("samples")
        if not isinstance(samples, list) or not samples:
            report.error(
                bank.label, where, "答案", "rubric.samples 缺失（输入/输出样例，仅教师端）"
            )
        else:
            if len(samples) < 2:
                report.warn(
                    bank.label,
                    where,
                    "答案",
                    f"只有 {len(samples)} 组样例：建议 ≥2 组（至少一组是边界），"
                    "学生和阅卷人都靠它对齐输出格式",
                )
            for i, sample in enumerate(samples):
                if not isinstance(sample, dict) or "input" not in sample or "output" not in sample:
                    report.error(
                        bank.label, where, "答案", f"第 {i + 1} 个样例必须是 {{input, output}}"
                    )
    elif qtype == "essay":
        if answer is not None and not isinstance(answer, dict):
            report.warn(bank.label, where, "答案", "essay 的 answer 建议省略或写成对象")


def check_body_kind(q: dict, where: str, report: Report, bank: Bank, qtype: str) -> None:
    body = q.get("body")
    if not isinstance(body, dict):
        report.error(bank.label, where, "结构", "body 必须是对象")
        return
    kind = body.get("kind")
    allowed = {
        "single": "options",
        "multiple": "options",
        "judge": "options",
        "fill": "template",
        "fix": "code",
        "code": "text",
    }
    want = allowed.get(qtype)
    if want and kind != want:
        hint = (
            "（代码简答题必须用 body.kind=text，不能借用改错题的 code body）"
            if qtype == "code"
            else ""
        )
        report.error(
            bank.label,
            where,
            "结构",
            f"type={qtype} 的 body.kind 必须是 {want}，实际 {kind!r}{hint}",
        )
    if qtype in ("single", "multiple", "judge"):
        keys = option_keys(q)
        texts = option_texts(q)
        if len(keys) < 2:
            report.error(bank.label, where, "结构", f"选项只有 {len(keys)} 个，至少 2 个")
        if len(set(keys)) != len(keys):
            report.error(bank.label, where, "结构", "选项键重复")
        if len(keys) > 26:
            report.error(bank.label, where, "结构", f"选项 {len(keys)} 个，超过 26 个（A–Z）")
        if len(set(texts)) != len(texts):
            report.error(bank.label, where, "结构", "选项文本重复，学生端无法区分")
        if keys:
            want_keys = [chr(ord("A") + i) for i in range(len(keys))]
            if keys != want_keys:
                report.warn(
                    bank.label,
                    where,
                    "结构",
                    f"选项键建议从 A 连续（期望 {want_keys}，实际 {keys}）",
                )
        if qtype in ("single", "multiple") and len(keys) != 4:
            report.warn(
                bank.label, where, "结构", f"本项目选择题统一 4 个选项，当前 {len(keys)} 个"
            )
    elif qtype == "fill":
        if not str(body.get("template") or "").strip():
            report.error(bank.label, where, "结构", "template 为空")
    elif qtype == "fix":
        code = body.get("code")
        if not isinstance(code, list) or not code:
            report.error(bank.label, where, "结构", "body.code 必须是非空数组（逐行代码）")
        elif not all(isinstance(x, str) for x in code):
            report.error(bank.label, where, "结构", "body.code 的每个元素必须是字符串")


def check_question(bank: Bank, index: int, report: Report, opts: Options) -> None:
    q = bank.questions[index]
    if not isinstance(q, dict):
        return
    qid = str(q.get("id") or "")
    where = f"#{index + 1} {qid or '<无 id>'}"
    raw_type = q.get("type")
    qtype = normalize_type(raw_type)
    legacy = bool(q.get("_legacy"))

    if not qid.strip():
        report.error(bank.label, where, "结构", "缺少 id")
    stem = q.get("stem")
    if not isinstance(stem, str) or not stem.strip():
        report.error(bank.label, where, "结构", "题干为空")
    if qtype not in ALLOWED_TYPES:
        report.error(
            bank.label,
            where,
            "结构",
            f"非法 type={raw_type!r}；支持 {', '.join(ALLOWED_TYPES)}"
            "（旧别名 multi/blank/blanks/fix_code 会被自动迁移）",
        )
        return
    if isinstance(raw_type, str) and raw_type != qtype:
        report.warn(bank.label, where, "结构", f"type={raw_type!r} 是旧别名，建议直接写 {qtype!r}")

    kp = q.get("kp")
    if not isinstance(kp, str) or not kp.strip():
        if legacy:
            report.warn(bank.label, where, "结构", "缺少 kp（旧格式允许；原生题库必填）")
        else:
            report.error(bank.label, where, "结构", "缺少 kp（形如 1.1–12.9）")
    elif not KP_RE.fullmatch(kp.strip()):
        report.error(bank.label, where, "结构", f"kp={kp!r} 不是 N.M（1.1–12.9）")

    difficulty = q.get("difficulty")
    if not is_int(difficulty):
        report.error(bank.label, where, "难度", f"difficulty 必须是整数，实际 {difficulty!r}")
    else:
        if qtype == "multiple":
            if difficulty != MULTIPLE_DIFFICULTY and not q.get("_legacy_multi"):
                report.error(
                    bank.label,
                    where,
                    "难度",
                    f"多选 difficulty 必须固定为 {MULTIPLE_DIFFICULTY}"
                    f"（哨兵值，不参与 1–3 统计），实际 {difficulty}",
                )
            elif q.get("_legacy_multi") and difficulty != MULTIPLE_DIFFICULTY:
                report.warn(
                    bank.label,
                    where,
                    "难度",
                    "旧格式用 difficulty=0 表示多选；迁移到原生格式时须写 9",
                )
        elif qtype == "code":
            if opts.big_difficulty:
                if difficulty != opts.big_difficulty:
                    report.error(
                        bank.label,
                        where,
                        "难度",
                        f"大题统一难度 {opts.big_difficulty}，实际 {difficulty}"
                        "（--big-difficulty 0 可关闭该检查）",
                    )
            elif difficulty not in (1, 2, 3, 4):
                report.error(
                    bank.label, where, "难度", f"大题 difficulty 必须在 1–4，实际 {difficulty}"
                )
        elif difficulty not in DIFF_NEAR_LEVELS:
            report.error(
                bank.label,
                where,
                "难度",
                f"{qtype} 的 difficulty 必须在 1/2/3，实际 {difficulty}",
            )

    score = q.get("score")
    if score is None:
        # 产品层允许省略（默认 2 分），所以这里只提醒；但本项目要求每题显式写分值，
        # 否则"总分多少"要靠数一遍才知道。
        report.warn(
            bank.label, where, "结构", "没写 score（产品按默认 2 分导入）：本项目要求显式写"
        )
    elif not is_int(score):
        report.error(bank.label, where, "结构", f"score 必须是整数，实际 {score!r}")
    elif not (0 <= score <= 1000):
        report.error(bank.label, where, "结构", f"score={score} 超出 0–1000")
    elif qtype == "code" and opts.big_score and score != opts.big_score:
        report.error(
            bank.label,
            where,
            "结构",
            f"大题统一 {opts.big_score} 分，实际 {score}（--big-score 0 可关闭该检查）",
        )

    check_body_kind(q, where, report, bank, qtype)
    check_answer(q, where, report, bank, qtype)
    check_language(q, where, report, bank, qtype, opts)
    check_judge_decl(q, where, report, bank, qtype)
    check_code_layout(q, where, report, bank)
    check_atomic(q, where, report, bank)
    check_meta_scoring(q, where, report, bank, qtype)


# ---------------------------------------------------------------- 库级校验


def check_duplicates(bank: Bank, report: Report, opts: Options) -> None:
    questions = [q for q in bank.questions if isinstance(q, dict)]
    label = bank.label
    exact = Counter(compact_stem(q.get("stem") or "") for q in questions)
    dups = [s for s, n in exact.items() if n > 1 and s]
    if dups:
        report.error(
            label,
            "全库",
            "重复",
            f"题干逐字重复 {len(dups)} 组（仅去空白后比较）",
        )
    families = Counter(normalize_stem(q.get("stem") or "") for q in questions)
    if families:
        worst, count = families.most_common(1)[0]
        if count > opts.max_skeleton_family:
            report.error(
                label,
                "全库",
                "重复",
                f"同一骨架 {count} 题（上限 {opts.max_skeleton_family}）：{worst[:40]}…",
            )
        is_choice = (
            sum(1 for q in questions if normalize_type(q.get("type")) == "single")
            >= len(questions) / 2
        )
        need = 120 if is_choice else 30
        need = min(need, len(questions))
        if len(families) < need:
            report.error(
                label,
                "全库",
                "重复",
                f"骨架种类只有 {len(families)} 种（下限 {need}）",
            )


def flush_meta_warnings(bank: Bank, report: Report) -> None:
    extra = len(bank.meta_missing) - META_WARN_LIMIT
    if extra > 0:
        report.warn(
            bank.label,
            "全库",
            "难度",
            f"另有 {extra} 题没有 meta.scoring（共 {len(bank.meta_missing)} 题）——"
            "逐条只报了前 " + str(META_WARN_LIMIT) + " 条，其余按同一规则处理",
        )


def check_distribution(bank: Bank, report: Report, opts: Options) -> None:
    if opts.limit:
        return  # --limit 只取前 N 题，分档下限没有意义
    questions = [q for q in bank.questions if isinstance(q, dict)]
    label = bank.label
    levels: Counter = Counter()
    by_type: dict[str, Counter] = {}
    for q in questions:
        qtype = normalize_type(q.get("type"))
        difficulty = q.get("difficulty")
        if not is_int(difficulty):
            continue
        if qtype in ("single", "multiple", "judge", "fill", "fix"):
            levels[difficulty] += 1
        by_type.setdefault(qtype, Counter())[difficulty] += 1
    total = sum(levels.values())
    for qtype in ("fill", "fix"):
        if qtype not in by_type or not opts.min_per_level:
            continue
        counts = by_type[qtype]
        for level in DIFF_NEAR_LEVELS:
            if counts.get(level, 0) < opts.min_per_level:
                report.error(
                    label,
                    "全库",
                    "分布",
                    f"{qtype} 难度 {level} 只有 {counts.get(level, 0)} 题"
                    f"（下限 {opts.min_per_level}；--min-per-level 0 可关闭）",
                )
    if total >= 20:
        top_level, top_count = levels.most_common(1)[0]
        if top_count / total > 0.60:
            report.warn(
                label,
                "全库",
                "分布",
                f"难度 {top_level} 占 {top_count}/{total}（>60%），分布偏斜",
            )
    if any(normalize_type(q.get("type")) == "code" for q in questions):
        codes = [q for q in questions if normalize_type(q.get("type")) == "code"]
        diffs = Counter(q.get("difficulty") for q in codes)
        if len(diffs) > 1:
            report.warn(label, "全库", "分布", f"大题难度不统一：{dict(diffs)}")


def check_answer_letters(bank: Bank, report: Report) -> None:
    """单选答案字母是否均衡。

    两套数字，别混用：
    * **出题目标**：450 道单选里每个字母 100–125 题（22%–28%）；
    * **门禁**：只拦明显失衡（<18% 或 >32%）。加题/减题后目标区间会被稀释，
      按 18%–32% 卡才不会因为"题库变大了"就误报。

    只在 ≥40 道单选时才判——样本太小时百分比没有意义（2 题里 1 个 A 就是 50%）。
    """
    singles = [
        q
        for q in bank.questions
        if isinstance(q, dict) and normalize_type(q.get("type")) == "single"
    ]
    if len(singles) < LETTER_MIN_SAMPLE:
        return
    letters: Counter = Counter()
    for q in singles:
        answer = q.get("answer") if isinstance(q.get("answer"), dict) else {}
        value = answer.get("answer")
        if isinstance(value, str) and value:
            letters[value] += 1
    total = sum(letters.values())
    if not total:
        return
    low, high = LETTER_SHARE_RANGE
    bad = []
    for key in "ABCD":
        share = letters.get(key, 0) / total
        if not (low <= share <= high):
            bad.append(f"{key}={letters.get(key, 0)}（{share:.1%}）")
    if bad:
        report.error(
            bank.label,
            "全库",
            "答案分布",
            f"单选答案字母失衡：{'、'.join(bad)}；门禁区间 {low:.0%}–{high:.0%}"
            f"（出题目标是每项 22%–28%）。学生在按位置蒙答案",
        )
    cycle = sum(
        1
        for i, q in enumerate(singles)
        if isinstance((q.get("answer") or {}).get("answer"), str)
        and (q.get("answer") or {}).get("answer") == "ABCD"[i % 4]
    )
    if cycle / total > CYCLE_SHARE_MAX:
        report.warn(
            bank.label,
            "全库",
            "答案分布",
            f"答案与 ABCD 循环吻合 {cycle}/{total}（>{CYCLE_SHARE_MAX:.0%}），疑似按位置排答案",
        )


def check_cross_bank(banks: list[Bank], report: Report) -> None:
    for i in range(len(banks)):
        for j in range(i + 1, len(banks)):
            left, right = banks[i], banks[j]
            a = {compact_stem(q.get("stem") or "") for q in left.questions if isinstance(q, dict)}
            b = {compact_stem(q.get("stem") or "") for q in right.questions if isinstance(q, dict)}
            same = {s for s in (a & b) if s}
            if same:
                report.error(
                    f"{left.label}/{right.label}",
                    "跨库",
                    "重复",
                    f"跨库逐字同题干 {len(same)} 组（同一门课不应出现两遍）",
                )


# ---------------------------------------------------------------- 编译回放


def fill_code(q: dict) -> tuple[str | None, str | None]:
    """把 fill 的模板按官方答案填好，拼上 harness。"""
    body = q.get("body") if isinstance(q.get("body"), dict) else {}
    template = str(body.get("template") or "")
    answer = q.get("answer") if isinstance(q.get("answer"), dict) else {}
    items = answer.get("answer") or []
    code = template
    for i, item in enumerate(items):
        mark = f"____{BLANK_CHARS[i]}____"
        text = str((item or {}).get("answer") if isinstance(item, dict) else item)
        if mark not in code:
            return None, f"模板里没有占位符 {mark}"
        code = code.replace(mark, text)
    return code, None


def fix_code(q: dict, fixed_lines: Iterable[int]) -> tuple[str | None, str | None]:
    """按给定行号应用改正，返回整段代码。"""
    body = q.get("body") if isinstance(q.get("body"), dict) else {}
    lines = [str(x) for x in (body.get("code") or [])]
    answer = q.get("answer") if isinstance(q.get("answer"), dict) else {}
    fixes = answer.get("answer") or []
    target = set(fixed_lines)
    for i, fix in enumerate(fixes):
        if not isinstance(fix, dict):
            return None, "改正项不是对象"
        if (i + 1) not in target:
            continue
        line = fix.get("line")
        if not is_int(line) or not (1 <= line <= len(lines)):
            return None, f"改正项 line={line!r} 越界"
        lines[line - 1] = str(fix.get("answer") or "")
    return "\n".join(lines), None


def replay_one(bank: Bank, index: int, compiler: Compiler, opts: Options) -> list[Finding]:
    """对单题做编译回放，返回发现（并行执行，不能直接读 Report）。"""
    q = bank.questions[index]
    findings: list[Finding] = []
    qtype = normalize_type(q.get("type"))
    qid = str(q.get("id") or "")
    where = f"#{index + 1} {qid or '<无 id>'}"

    def err(rule: str, message: str) -> None:
        findings.append(Finding(ERROR, bank.label, where, rule, message))

    judge = q.get("judge") if isinstance(q.get("judge"), dict) else None
    if judge is None:
        return findings
    cases = [c for c in (judge.get("tests") or []) if isinstance(c, dict)]
    harness = str(judge.get("harness") or "")
    if not cases or not harness:
        return findings

    if qtype == "fill":
        code, problem = fill_code(q)
        if problem:
            err("判题", f"无法拼出可编译代码：{problem}")
            return findings
        full = f"{PRE}{code}\n\n{harness}\n"
        reason = run_cases(compiler, full, cases, f"fill_{qid}")
        if reason:
            err("判题", f"官方答案回放失败：{reason}")
        return findings

    if qtype == "fix":
        answer = q.get("answer") if isinstance(q.get("answer"), dict) else {}
        fixes = answer.get("answer") or []
        fixed, problem = fix_code(q, range(1, len(fixes) + 1))
        if problem:
            err("判题", f"无法拼出修正后的代码：{problem}")
            return findings
        reason = run_cases(compiler, f"{PRE}{fixed}\n\n{harness}\n", cases, f"fix_{qid}")
        if reason:
            err("判题", f"官方修正后回放失败：{reason}")
        original, problem = fix_code(q, [])
        if problem:
            err("判题", f"无法拼出原始代码：{problem}")
            return findings
        if run_cases(compiler, f"{PRE}{original}\n\n{harness}\n", cases, f"orig_{qid}") is None:
            err("判题", "原始（未改）代码也能通过全部用例：这道题没有错可改")
        for skip in range(1, len(fixes) + 1):
            partial, problem = fix_code(q, [i for i in range(1, len(fixes) + 1) if i != skip])
            if problem:
                err("判题", f"撤销第 {skip} 处后无法拼出代码：{problem}")
                continue
            if (
                run_cases(compiler, f"{PRE}{partial}\n\n{harness}\n", cases, f"part_{qid}_{skip}")
                is None
            ):
                err(
                    "判题",
                    f"撤销第 {skip} 处修改后仍通过全部用例：该处不可测（改没改都得分）",
                )
    return findings


def run_replay(
    banks: list[Bank], compiler: Compiler, report: Report, opts: Options, jobs: int
) -> dict[str, int]:
    """逐题回放 fill/fix。

    返回 ``{题库标签: 通过题数, 题库标签+"__total": 总题数}``，供人读/JSON 输出
    里显示"回放 60/60"——只写 PASS 而不说清回放跑了几题，读的人没法判断
    判题材料到底验过没有。
    """
    tasks: list[tuple[Bank, int]] = []
    for bank in banks:
        for index, q in enumerate(bank.questions):
            if not isinstance(q, dict):
                continue
            if normalize_type(q.get("type")) in ("fill", "fix"):
                tasks.append((bank, index))
    stats: dict[str, int] = {}
    if not tasks:
        return stats
    for bank, _index in tasks:
        stats.setdefault(bank.label, 0)
        stats[bank.label + "__total"] = stats.get(bank.label + "__total", 0) + 1

    workers = jobs if jobs > 0 else min(8, os.cpu_count() or 4)
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
        futures = {
            pool.submit(replay_one, bank, index, compiler, opts): (bank, index)
            for bank, index in tasks
        }
        for future in concurrent.futures.as_completed(futures):
            bank, index = futures[future]
            try:
                findings = future.result()
            except Exception as exc:  # noqa: BLE001 - 单题异常不能拖垮整轮门禁
                report.error(bank.label, f"#{index + 1}", "判题", f"回放时异常：{exc}")
                continue
            for finding in findings:
                report.items.append(finding)
            if not findings:
                stats[bank.label] = stats.get(bank.label, 0) + 1
    return stats


# ---------------------------------------------------------------- 输出


def fmt_dist(counter: Counter) -> str:
    if not counter:
        return "—"
    parts = []
    for key in sorted(counter, key=lambda x: (str(type(x)), x if isinstance(x, int) else 0)):
        parts.append(f"{key}:{counter[key]}")
    return " ".join(parts)


def parse_expect(spec: str) -> dict[str, int]:
    result: dict[str, int] = {}
    if not spec.strip():
        return result
    for chunk in spec.split(","):
        text = chunk.strip()
        if not text:
            continue
        if "=" not in text:
            raise ValueError(f"--expect 的每一项都要写成 名字=题数，实际 {text!r}")
        name, _, raw = text.partition("=")
        name = name.strip()
        try:
            value = int(raw.strip())
        except ValueError as exc:
            raise ValueError(f"--expect 里 {name!r} 的题数不是整数：{raw!r}") from exc
        if value < 0:
            raise ValueError(f"--expect 里 {name!r} 的题数不能为负")
        if not name:
            raise ValueError(f"--expect 里有空名字：{text!r}")
        result[name] = value
    return result


def bank_names(bank: Bank) -> set[str]:
    """一个题库能被 ``--expect`` 叫到的名字：文件名主干、文件名、exam_id。"""
    return {
        bank.path.stem,
        bank.path.name,
        str(bank.data.get("exam_id") or ""),
    } - {""}


def apply_expect(
    banks: list[Bank], expect: dict[str, int], report: Report, limit: int = 0
) -> None:
    """按 ``--expect`` 核对题数。

    名字先精确匹配（主干 / 文件名 / ``exam_id``）；都不中时按"主干前缀"匹配，
    于是 ``rutuan=30`` 能命中 ``rutuan-q16-19.json``——题库文件名带年份/轮次后缀是
    常态，要求用户把后缀抄全只会写错。一个名字命中多个题库算 error（有歧义）。
    """
    for name, want in expect.items():
        hits = [bank for bank in banks if name in bank_names(bank)]
        if not hits:
            hits = [
                bank
                for bank in banks
                if any(
                    other.startswith(name + "-") or other.startswith(name + "_")
                    for other in bank_names(bank)
                )
            ]
        if not hits:
            report.error(
                name,
                "顶层",
                "题数",
                f"--expect 指定了 {name}={want}，但没找到对应题库"
                f"（现有：{', '.join(sorted(b.label for b in banks)) or '无'}）",
            )
            continue
        if len(hits) > 1:
            report.error(
                name,
                "顶层",
                "题数",
                f"--expect 的 {name} 同时命中 {', '.join(sorted(b.label for b in hits))}："
                "请写全文件名主干",
            )
            continue
        bank = hits[0]
        got = len(bank.questions)
        total = bank.total_questions or got
        if limit and total != got:
            continue  # --limit 冒烟模式不判题数
        if got != want:
            report.error(bank.label, "顶层", "题数", f"题数 {got} != 期望 {want}")


def print_human(
    banks: list[Bank],
    report: Report,
    notes: list[str],
    expect: dict[str, int],
    limit: int,
    stats: dict[str, int] | None = None,
    replay_state: str = "skip",
) -> None:
    if expect:
        spec = ", ".join(f"{k}={v}" for k, v in expect.items())
        print(f"期望题数：{spec}")
    stats = stats or {}
    for bank in banks:
        types = Counter(
            normalize_type(q.get("type")) for q in bank.questions if isinstance(q, dict)
        )
        levels = Counter(q.get("difficulty") for q in bank.questions if isinstance(q, dict))
        tag = "（旧格式·内存转换）" if bank.legacy else ""
        line = (
            f"[{bank.label}] {len(bank.questions)} 题{tag}  "
            f"题型 {fmt_dist(types)}  难度 {fmt_dist(levels)}"
        )
        total = stats.get(bank.label + "__total")
        if total:
            line += f"  判题回放 {stats.get(bank.label, 0)}/{total}"
        print(line)
        for note in bank.notes:
            print(f"    · {note}")
    for note in notes:
        print(f"    · {note}")
    if replay_state == "skip":
        print("    · 判题回放：已跳过（--no-compile，只证明结构与排版）")
    elif replay_state in ("warn", "missing"):
        print("    · 判题回放：未执行（本机没有 C 编译器）")
    errors = report.errors
    warnings = report.warnings
    print()
    for finding in errors[:limit]:
        print(finding.render())
    if len(errors) > limit:
        print(f"... 另有 {len(errors) - limit} 条 ERROR 未显示（--max-report 调整）")
    for finding in warnings[:limit]:
        print(finding.render())
    if len(warnings) > limit:
        print(f"... 另有 {len(warnings) - limit} 条 WARN 未显示（--max-report 调整）")
    print()
    print(f"题库 {len(banks)} 个，题 {sum(len(b.questions) for b in banks)} 道")
    print(f"errors={len(errors)} warnings={len(warnings)}")
    print("PASS" if not errors else "FAIL")


def print_json(
    banks: list[Bank],
    report: Report,
    notes: list[str],
    stats: dict[str, int] | None = None,
    replay_state: str = "skip",
) -> None:
    stats = stats or {}
    payload = {
        "ok": not report.errors,
        "errors": len(report.errors),
        "warnings": len(report.warnings),
        "replay": {
            "state": replay_state,
            "passed": {k: v for k, v in stats.items() if not k.endswith("__total")},
            "total": {k[: -len("__total")]: v for k, v in stats.items() if k.endswith("__total")},
        },
        "banks": [
            {
                "label": bank.label,
                "path": str(bank.path),
                "legacy": bank.legacy,
                "questions": len(bank.questions),
                "types": dict(
                    Counter(
                        normalize_type(q.get("type")) for q in bank.questions if isinstance(q, dict)
                    )
                ),
                "difficulty": {
                    str(k): v
                    for k, v in Counter(
                        q.get("difficulty") for q in bank.questions if isinstance(q, dict)
                    ).items()
                },
                "notes": bank.notes,
            }
            for bank in banks
        ],
        "notes": notes,
        "findings": [
            {
                "severity": f.severity,
                "scope": f.scope,
                "where": f.where,
                "rule": f.rule,
                "message": f.message,
            }
            for f in report.items
        ],
    }
    print(json.dumps(payload, ensure_ascii=False, indent=2))


# ---------------------------------------------------------------- 入口


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="verify_bank.py",
        description=(
            "题库门禁：只读校验 JSON 题库（原生格式；成对的 format_version=2 旧格式也可）。"
            " 有 error 时退出码 1；只有 warning 时退出码 0。"
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "示例：\n"
            "  python verify_bank.py banks/final --expect choice=500,fill=60,fix=61\n"
            "  python verify_bank.py banks/final --no-compile\n"
            "  python verify_bank.py banks/final --clang /path/to/clang --jobs 8\n"
            "  python verify_bank.py banks/final/a.json --json\n"
            "  python verify_bank.py banks/final --require-compile   # CI：没编译器就算失败\n"
        ),
    )
    parser.add_argument(
        "paths",
        nargs="*",
        metavar="PATH",
        help="题库文件或目录，可多个；目录取其中 *.json；默认当前目录",
    )
    parser.add_argument(
        "--recursive", action="store_true", help="目录递归查找 *.json（默认只看一层）"
    )
    parser.add_argument(
        "--expect",
        metavar="SPEC",
        default="",
        help="期望题数，如 choice=500,fill=60,fix=61；名字按文件名主干、exam_id"
        " 或主干前缀（rutuan 命中 rutuan-q16-19）匹配",
    )
    parser.add_argument(
        "--no-compile", action="store_true", help="跳过 fill/fix 的编译回放（结构检查照常）"
    )
    parser.add_argument(
        "--clang",
        metavar="PATH",
        nargs="?",
        const="",
        default=None,
        help="C 编译器。给路径就用它；只写 --clang 表示优先用 PATH 里的 clang；"
        "不给则依次查 $CLANG、$CC、PATH 里的 clang/gcc/cc",
    )
    parser.add_argument(
        "--require-compile",
        action="store_true",
        help="找不到编译器时算 error（默认只报警告：题库没问题，是本机缺工具链）",
    )
    parser.add_argument("--jobs", type=int, default=0, help="编译回放并发数，默认 min(8, CPU)")
    parser.add_argument(
        "--min-per-level",
        type=int,
        default=15,
        help="fill/fix 每档最少题数（默认 15；0 关闭）",
    )
    parser.add_argument(
        "--max-skeleton-family",
        type=int,
        default=4,
        help="同一骨架最多几题（默认 4；0 关闭）",
    )
    parser.add_argument(
        "--big-difficulty",
        type=int,
        default=3,
        help="大题统一难度（默认 3；0 不检查）",
    )
    parser.add_argument(
        "--big-score", type=int, default=15, help="大题统一分值（默认 15；0 不检查）"
    )
    parser.add_argument(
        "--big-lang",
        default="c",
        help='大题必须声明 code_languages:["c"]（默认 c；空串不检查）',
    )
    parser.add_argument(
        "--limit", type=int, default=0, help="每库只校验前 N 题（冒烟用；0 = 全部）"
    )
    parser.add_argument(
        "--max-report", type=int, default=80, help="人读模式下最多打印多少条（默认 80）"
    )
    parser.add_argument("--json", action="store_true", help="输出 JSON 报告（便于 CI 解析）")
    return parser


def main(argv: list[str] | None = None) -> int:
    force_utf8_when_piped()
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        expect = parse_expect(args.expect)
    except ValueError as exc:
        print(f"参数错误：{exc}", file=sys.stderr)
        return 2

    opts = Options(
        min_per_level=max(0, args.min_per_level),
        max_skeleton_family=args.max_skeleton_family,
        big_difficulty=args.big_difficulty,
        big_score=args.big_score,
        big_lang=args.big_lang,
        limit=max(0, args.limit),
    )
    if not opts.max_skeleton_family:
        opts.max_skeleton_family = 10**9

    report = Report()
    try:
        files = discover([Path(p) for p in (args.paths or ["."])], args.recursive)
    except UsageError as exc:
        print(f"错误：{exc}", file=sys.stderr)
        return 2
    if not files:
        print("错误：给定路径下没有找到 *.json 题库文件", file=sys.stderr)
        return 2

    try:
        banks, notes = load_banks(files, report, opts)
    except UsageError as exc:
        print(f"错误：{exc}", file=sys.stderr)
        return 2

    for bank in banks:
        check_top(bank, report, opts)
        for index in range(len(bank.questions)):
            check_question(bank, index, report, opts)
        check_duplicates(bank, report, opts)
        check_distribution(bank, report, opts)
        check_answer_letters(bank, report)
        flush_meta_warnings(bank, report)
    check_cross_bank(banks, report)
    apply_expect(banks, expect, report, opts.limit)

    workdir = Path(tempfile.mkdtemp(prefix="bank_qc_"))
    stats: dict[str, int] = {}
    replay_state = "skip"
    try:
        if not args.no_compile:
            compiler = Compiler(Compiler.discover(args.clang), workdir)
            if not compiler.available:
                # 题库本身没有问题，是本机缺工具链：默认只报警告。
                # CI 想"没编译器就别放行"时加 --require-compile。
                message = (
                    "找不到 C 编译器（clang/gcc/cc 都不在 PATH，$CLANG/$CC 也没设置），"
                    "fill/fix 的判题回放没有执行：用 --clang 指定编译器路径，"
                    "或把它加进 PATH；只想做结构检查就加 --no-compile"
                )
                if args.require_compile:
                    report.error("全局", "判题", "工具链", message)
                    replay_state = "missing"
                else:
                    report.warn("全局", "判题", "工具链", message + "（本次按 warning 放行）")
                    replay_state = "warn"
            else:
                has_flatten = any(
                    normalize_type(q.get("type")) in ("fill", "fix")
                    for bank in banks
                    for q in bank.questions
                    if isinstance(q, dict)
                )
                toolchain_problem = check_compiler(compiler) if has_flatten else None
                if toolchain_problem:
                    message = (
                        f"编译器 {compiler.exe} 连一个最小程序都编译不了：{toolchain_problem}；"
                        "判题回放已跳过（这是本机工具链的问题，不是题库的问题）"
                    )
                    if args.require_compile:
                        report.error("全局", "判题", "工具链", message)
                        replay_state = "missing"
                    else:
                        report.warn("全局", "判题", "工具链", message + "（本次按 warning 放行）")
                        replay_state = "warn"
                elif has_flatten:
                    print(
                        f"编译回放中（{compiler.name}={compiler.exe}，"
                        f"并发 {args.jobs or 'auto'}）…",
                        file=sys.stderr,
                    )
                    stats = run_replay(banks, compiler, report, opts, args.jobs)
                    replay_state = "done"
    finally:
        shutil.rmtree(workdir, ignore_errors=True)

    if args.json:
        print_json(banks, report, notes, stats, replay_state)
    else:
        print_human(banks, report, notes, expect, max(1, args.max_report), stats, replay_state)
    return 0 if not report.errors else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        print("已中断", file=sys.stderr)
        sys.exit(130)
