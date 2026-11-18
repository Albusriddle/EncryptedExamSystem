# -*- coding: utf-8 -*-
"""题库质检补充门禁（question-bank-qc 的配套脚本，只读题库目录）。

用法：
    python qc.py <题库目录> [--no-compile] [--blank-audit] [--strict-kp]
                 [--report OUT.md] [--verify PATH] [--ids F-0001,F-0029]

它做两件事：
1. 调用同目录（或 --verify 指定）的 verify.py 跑正式门禁，原样打印并计入结论；
2. 补 verify.py **不查**的 8 类项（编号 V1–V8，定义见本文件下文）：
   V1 选项可见性：去空白/折叠空白后同形（尾随空格陷阱不可见）、NBSP/零宽字符
   V2 代码排版：围栏语言、int main 单行、{ } 独占行、行内代码写程序
   V3 句式族：标识符掩码后同一壳 >4 题（随机函数名掩盖的生成器痕迹）
   V4 难度账本：缺整数 meta.scoring（REVIEW）、多选真实 S 越界（error）、C 标注与行数数量级矛盾（REVIEW）
   V5 kp：越纲 kp、51 点覆盖缺口
   V6 pick / exam_id：唯一性、文件名安全、抽题比例
   V7 judge.harness 写当前目录（fopen/remove/rename/system…）
   V8 fill 空位未钉死（错误值注入；--blank-audit 时实跑，一律 REVIEW：由复核人判“域内等价/真漏判”）

退出码：0 = 0 error（仍需逐条裁决 REVIEW）；1 = 有 error；2 = 用法/环境错误。
题库目录只读；所有临时文件落在系统 temp。
"""
from __future__ import annotations

import argparse
import glob
import importlib.util
import io
import json
import os
import re
import subprocess
import sys
import tempfile
from collections import Counter, defaultdict

STD_FILES = {
    "choice": ("choice.json", 500),
    "fill": ("fill.json", 60),
    "fix": ("fix.json", 61),
    "rutuan": ("rutuan-q16-19.json", 30),
    "zhaosheng": ("zhaosheng-b-q16-19.json", 30),
}
KPS51 = {
    "1.1", "1.2", "1.3", "1.4", "1.5", "2.1", "2.2", "2.3", "2.4", "2.5", "2.6",
    "3.1", "3.2", "3.3", "4.1", "4.2", "4.3", "4.4", "5.1", "5.2", "5.3", "5.4", "5.5",
    "6.1", "6.2", "6.3", "6.4", "6.5", "6.6", "6.7", "7.1", "7.2", "7.3", "7.4",
    "8.1", "8.2", "8.3", "8.4", "9.1", "9.2", "9.3", "10.1", "10.2", "10.3",
    "11.1", "11.2", "11.3", "11.4", "11.5", "12.1", "12.2",
}
RESERVED = {"con", "prn", "aux", "nul"} | {f"com{i}" for i in range(1, 10)} | {f"lpt{i}" for i in range(1, 10)}
BLANK_RE = re.compile(r"____([①②③④⑤⑥⑦⑧⑨⑩])____")
FENCE_RE = re.compile(r"```([A-Za-z]*)[ \t]*\n?(.*?)```", re.S)
INLINE_RE = re.compile(r"`([^`\n]*)`")
WS_RE = re.compile(r"\s+")
INVIS = {"\u00a0": "NBSP", "\u3000": "全角空格", "\u200b": "零宽空格",
         "\u200c": "ZWNJ", "\u200d": "ZWJ", "\ufeff": "BOM", "\t": "TAB"}
#: 填空错误值候选（按官方答案精确匹配；另附整数 ±1）
CAND = {
    "<": ["<="], "<=": ["<"], ">": [">="], ">=": [">"], "==": ["<="], "!=": ["=="],
    "&&": ["&", "||"], "&": ["&&"], "||": ["|"], "|": ["||"],
    "+": ["-", "|", "^"], "-": ["+", "^"], "*": ["/"], "/": ["*", "%"], "%": ["/"],
    "++": ["--"], "--": ["++"], "i": ["i - 1"], "n - 1": ["n - 2"], "n-1": ["n-2"],
    "0": ["1"], "1": ["0"],
}
HARNESS_WRITE = re.compile(r"fopen|freopen|remove\s*\(|rename\s*\(|system\s*\(|tmpnam|fwrite\s*\(|fputs?\s*\(")


class Ctx:
    def __init__(self):
        self.errors: list[str] = []
        self.warns: list[str] = []
        self.reviews: list[str] = []

    def err(self, m):
        self.errors.append(m)

    def warn(self, m):
        self.warns.append(m)

    def review(self, m):
        self.reviews.append(m)


def load_json(path):
    with io.open(path, encoding="utf-8-sig") as f:
        return json.load(f)


def qtype(q):
    return q.get("type") or q.get("qtype")


def options(q):
    return (q.get("body") or {}).get("options") or []


def code_blocks(stem):
    return [m.group(2) for m in FENCE_RE.finditer(stem or "")]


def mask_stem(stem):
    s = FENCE_RE.sub("<CODE>", stem or "")
    s = INLINE_RE.sub("<C>", s)
    s = re.sub(r"[A-Za-z_]\w*", "<I>", s)
    s = re.sub(r"\d+", "<N>", s)
    return WS_RE.sub("", s)


def vis_norm(text):
    t = text or ""
    for ch in INVIS:
        t = t.replace(ch, " ")
    return WS_RE.sub(" ", t).strip()


# ------------------------------------------------------------------ 载入题库

def expand_v2(q_payload, a_payload, name):
    """把 questions/answers 分离格式（format_version=2）展开成统一形状。"""
    qs = q_payload.get("questions") or []
    by_id = {str(a.get("id")): a for a in (a_payload or {}).get("answers") or []}
    out = []
    t = q_payload.get("type")
    for raw in qs:
        qid = str(raw.get("id"))
        ans = (by_id.get(qid) or {}).get("verdict") or {}
        content = raw.get("content") or {}
        kind = content.get("kind")
        if t == "choice":
            opts = [{"key": k, "text": str(v)} for k, v in (content.get("options") or {}).items()]
            body = {"kind": "options", "options": opts}
            typ = "multiple" if kind == "options" and ans.get("kind") == "multi" else "single"
            answer = {"kind": "single" if typ == "single" else "multi", "answer": ans.get("answer")}
        elif t == "fill_blank":
            body = {"kind": "template", "template": content.get("template", ""),
                    "blank_count": content.get("blank_count")}
            typ, answer = "fill", {"kind": "blanks", "answer": ans.get("answer")}
        else:
            body = {"kind": "code", "code": content.get("code") or [],
                    "blank_count": content.get("blank_count")}
            typ, answer = "fix", {"kind": "fixes", "answer": ans.get("answer")}
        out.append({"id": qid, "type": typ, "stem": raw.get("stem", ""), "body": body,
                    "kp": raw.get("kp"), "difficulty": raw.get("difficulty"),
                    "score": 2, "answer": answer,
                    "judge": (by_id.get(qid) or {}).get("judge")})
    return {"exam_id": name, "title": name, "pick": len(out), "questions": out, "file": name + " (v2)"}


def load_banks(root, ctx):
    banks = {}
    taken = set()
    for name, (fn, _n) in STD_FILES.items():
        p = os.path.join(root, fn)
        if not os.path.exists(p):
            continue
        try:
            d = load_json(p)
        except Exception as e:  # noqa: BLE001
            ctx.err(f"{fn}: JSON 解析失败 {e}")
            continue
        taken.add(os.path.abspath(p))
        d["file"] = fn
        banks[name] = d
    for p in sorted(glob.glob(os.path.join(root, "*.json"))):
        if os.path.abspath(p) in taken:
            continue
        try:
            d = load_json(p)
        except Exception:
            ctx.warn(f"{os.path.basename(p)}: 不是合法 JSON，跳过")
            continue
        if not isinstance(d, dict):
            continue
        if d.get("format_version") == 2:
            continue  # 统一在下面按对处理
        if "exam_id" in d and "questions" in d:
            name = "native:" + os.path.basename(p)
            d["file"] = os.path.basename(p)
            banks[name] = d
            taken.add(os.path.abspath(p))
    groups = defaultdict(dict)
    for p in sorted(glob.glob(os.path.join(root, "*.json"))):
        try:
            d = load_json(p)
        except Exception:
            continue
        if not isinstance(d, dict) or d.get("format_version") != 2:
            continue
        base = re.sub(r"\.(questions|answers)$", "", os.path.splitext(os.path.basename(p))[0])
        base = re.sub(r"\s*\(\d+\)$", "", base)
        groups[(os.path.dirname(p), base)][d.get("role")] = d
    for (_parent, base), pair in sorted(groups.items()):
        q = pair.get("questions")
        a = pair.get("answers")
        if not q or not a:
            ctx.err(f"{base}: 分离格式缺少 {'questions' if not q else 'answers'} 文件")
            continue
        banks["v2:" + base] = expand_v2(q, a, base)
    return banks


# ------------------------------------------------------------------ V1..V7

def check_options(bank, qs, ctx):
    """V1：选项在学生端不可分辨（尾随空格/全角空格/零宽字符/折叠后同形）。"""
    for i, q in enumerate(qs):
        opts = options(q)
        if not opts:
            continue
        wid = f"{bank}[{i}] {q.get('id')}"
        seen = {}
        for o in opts:
            raw = o.get("text") or ""
            key = vis_norm(raw)
            if key in seen:
                ctx.err(f"{wid}: 选项 {seen[key]} 与 {o.get('key')} 渲染后同形（仅差空白/不可见字符）")
            else:
                seen[key] = o.get("key")
                if raw != raw.strip():
                    ctx.warn(f"{wid}: 选项 {o.get('key')} 带首尾空白（客户端不渲染，属美观问题）")
            for ch, label in INVIS.items():
                if ch in raw:
                    ctx.warn(f"{wid}: 选项 {o.get('key')} 含不可见字符 {label}")


def check_typography(bank, qs, ctx):
    """V2：题干代码排版。verify.py 只拦最坏情况，这里补齐缩进与围栏语言。"""
    for i, q in enumerate(qs):
        stem = q.get("stem") or ""
        wid = f"{bank}[{i}] {q.get('id')}"
        for m in FENCE_RE.finditer(stem):
            lang, body = m.group(1), m.group(2)
            lines = [ln for ln in body.strip("\n").splitlines()]
            if lang.lower() not in ("", "c"):
                ctx.warn(f"{wid}: 代码围栏语言为 {lang!r}（题库统一 ```c）")
            if not [ln for ln in lines if ln.strip()]:
                ctx.err(f"{wid}: 空代码块（题干说“如下代码”却没有任何代码）")
                continue
            if "int main" in body and len(lines) == 1:
                ctx.err(f"{wid}: 完整程序被压成单行（客户端渲染成一条深色长条）")
            elif len(lines) == 1:
                one = body.strip()
                if len(one) > 40 and ";" in one:
                    ctx.err(f"{wid}: 单行围栏块 {len(one)} 字符过长，应换行（≤40 字符）")
            if "int main" in body:
                sig = next((k for k, ln in enumerate(lines) if "int main" in ln), 0)
                if lines[sig].strip().rstrip(") ").endswith("{"):
                    ctx.warn(f"{wid}: int main 的 {{ 未独占一行")
                if not any(ln.strip() == "{" for ln in lines):
                    ctx.warn(f"{wid}: int main 程序缺少独占一行的 {{")
                if not any(ln.strip() == "}" for ln in lines):
                    ctx.warn(f"{wid}: int main 程序缺少独占一行的 }}")
                # 只检查 main 花括号内部（main 之前的全局声明本来就顶格）
                try:
                    open_i = next(k for k in range(sig, len(lines)) if lines[k].strip() == "{")
                    close_i = next((k for k in range(open_i + 1, len(lines))
                                    if lines[k].strip() == "}"), len(lines))
                except StopIteration:
                    open_i, close_i = sig, len(lines)
                inner = lines[open_i + 1:close_i]
                bad_indent = [ln for ln in inner if ";" in ln and ln[:1] not in (" ", "\t")]
                if bad_indent:
                    ctx.warn(f"{wid}: main 内有 {len(bad_indent)} 行语句未缩进")
                if any("\t" in ln for ln in lines):
                    ctx.warn(f"{wid}: 代码使用 TAB 缩进（统一 4 空格）")
        no_fence = FENCE_RE.sub("", stem)
        for inline in INLINE_RE.findall(no_fence):
            if "int main" in inline:
                ctx.err(f"{wid}: 完整程序写了行内代码")
            elif len(inline) > 60 and ";" in inline and "{" in inline:
                ctx.err(f"{wid}: 程序片段写了行内代码（>60 字符且含 ;{{ }}）")
            elif len(inline) > 40 and ";" in inline:
                ctx.warn(f"{wid}: 行内代码 {len(inline)} 字符含分号，考虑改围栏块")


def check_skeletons(bank, qs, ctx):
    """V3：标识符掩码后的句式族（随机函数名会骗过 verify.py 的骨架统计）。"""
    fam = defaultdict(list)
    for q in qs:
        fam[mask_stem(q.get("stem"))].append(q.get("id"))
    big = [(k, v) for k, v in fam.items() if len(v) > 4]
    for k, ids in sorted(big, key=lambda kv: -len(kv[1]))[:5]:
        ctx.warn(f"{bank}: 去标识符后同一句式 {len(ids)} 题（上限 4）：{', '.join(map(str, ids[:8]))}…")
    return len(fam), max((len(v) for v in fam.values()), default=0)


def nonempty_code_lines(q):
    """C 的统计口径（difficulty.md §1）：题干围栏块 / body.template / body.code 的非空行。"""
    t = qtype(q)
    body = q.get("body") or {}
    if t == "fill":
        return len([ln for ln in (body.get("template") or "").splitlines() if ln.strip()])
    if t == "fix":
        return len([ln for ln in (body.get("code") or []) if str(ln).strip()])
    return len([ln for ln in code_blocks(q.get("stem")) for ln in ln.splitlines() if ln.strip()])


def check_difficulty(bank, qs, ctx):
    """V4：难度账本（K/A/C）。verify.py 只在 meta.scoring 存在时校验 S↔difficulty，且对多选完全跳过；
    这里补三件它不查的：缺整数标注、多选真实 S 越界（difficulty.md §2：MUST ∈[2.00,2.20]）、
    C 标注与实际非空代码行数量级矛盾（差 ≥2 档）。"""
    for q in qs:
        t, wid = qtype(q), f"{bank} {q.get('id')}"
        sc = (q.get("meta") or {}).get("scoring")
        if not isinstance(sc, dict) or any(
                isinstance(sc.get(k), bool) or not isinstance(sc.get(k), int) for k in ("k", "a", "c")):
            ctx.review(f"{wid}: 缺整数 meta.scoring（difficulty.md §4 要求每题必标，无法标注不得入库）")
            continue
        if t == "multiple":
            s = 0.35 * sc["k"] + 0.45 * sc["a"] + 0.20 * sc["c"]
            if not (2.00 - 1e-9 <= s <= 2.20 + 1e-9):
                ctx.err(f"{wid}: 多选真实 S={s:.2f}（K/A/C={sc['k']}/{sc['a']}/{sc['c']}）"
                        f"不在 [2.00, 2.20]；difficulty=9 只是展示标签，真实难度必须落在带内")
            continue
        if t == "code":
            continue  # 大题 C 按 answer.reference 的核心算法行由作者标注，复核抽检
        n = nonempty_code_lines(q)
        if n == 0 and t in ("single", "judge"):
            continue
        c = 1 if n <= 3 else 2 if n <= 8 else 3 if n <= 20 else 4
        if abs(c - sc["c"]) >= 2:
            ctx.review(f"{wid}: 非空代码 {n} 行对应 C={c}，标注 C={sc['c']}（差 ≥2 档，确认统计范围）")


def check_kp(banks, ctx, strict):
    seen_all = set()
    for bank, d in banks.items():
        qs = d.get("questions") or []
        c = Counter(q.get("kp") for q in qs if q.get("kp"))
        seen_all |= set(c)
        for kp, n in c.items():
            if kp not in KPS51:
                msg = f"{bank}: kp={kp} 不在 51 点大纲内（{n} 题）"
                ctx.err(msg) if strict else ctx.warn(msg + "（--strict-kp 记为 error）")
        # 旧规范（spec.md §2）的“每个 kp 至少覆盖 2 个难度档”已被 difficulty.md §3 的
        # “难度 1/2/3 每档 ≥15 题、总量不限”取代；一题一 kp 是原子化的必然结果，不再按 kp 报 REVIEW。
    missing = sorted(KPS51 - seen_all)
    if missing:
        ctx.warn(f"51 点未覆盖 {len(missing)} 个：{', '.join(missing)}")


def check_ids(banks, ctx):
    """V6：exam_id 唯一/文件名安全、pick 合法与比例。"""
    ids = {}
    for bank, d in banks.items():
        eid = str(d.get("exam_id") or "")
        if not eid:
            ctx.err(f"{bank}: exam_id 为空")
            continue
        if eid in ids:
            ctx.err(f"{bank} 与 {ids[eid]} 的 exam_id 相同（{eid}）：同一工程导入会互相覆盖")
        ids[eid] = bank
        if len(eid) > 64:
            ctx.err(f"{bank}: exam_id 过长（{len(eid)} > 64）")
        if any(c in '/\\:*?"<>|' for c in eid):
            ctx.err(f"{bank}: exam_id 含路径分隔符/Windows 禁用字符：{eid!r}")
        if any(ord(c) < 0x20 or c.isspace() for c in eid):
            ctx.err(f"{bank}: exam_id 含空白或控制字符：{eid!r}")
        if eid.startswith(".") or eid.endswith("."):
            ctx.err(f"{bank}: exam_id 以 . 开头/结尾：{eid!r}")
        if eid.lower() in RESERVED:
            ctx.err(f"{bank}: exam_id 是 Windows 保留名：{eid!r}")
        qs = d.get("questions") or []
        pick = d.get("pick")
        if not isinstance(pick, int) or not (0 < pick <= len(qs)):
            ctx.err(f"{bank}: pick={pick!r} 必须在 1..{len(qs)}")
            continue
        ratio = pick / max(1, len(qs))
        if bank in ("choice", "fill", "fix") and not (0.05 <= ratio <= 0.5):
            ctx.warn(f"{bank}: pick={pick}/{len(qs)}（{ratio:.0%}）不合常规卷面比例，确认考务口径")


def check_judge_io(banks, ctx):
    """V7：判题材料写当前目录（只读/并发判题会互相踩）。"""
    for bank, d in banks.items():
        for q in d.get("questions") or []:
            h = (q.get("judge") or {}).get("harness") or ""
            m = HARNESS_WRITE.search(h)
            if m:
                ctx.err(f"{bank} {q.get('id')}: harness 写文件（{m.group(0)}…），判题不得写 cwd")
            if "rand" in h or "time(" in h:
                ctx.warn(f"{bank} {q.get('id')}: harness 含 rand/time，判题不可复现")


# ------------------------------------------------------------------ V8

def run_all(cc, work, tag, code, tests):
    src = os.path.join(work, tag + ".c")
    exe = os.path.join(work, tag + ".exe")
    with io.open(src, "w", encoding="utf-8") as f:
        f.write(code)
    cp = subprocess.run([cc, "-O0", "-std=c11", "-w", "-o", exe, src],
                        capture_output=True, text=True, encoding="utf-8", errors="replace")
    if cp.returncode != 0:
        return "compile_error:" + (cp.stderr or "").strip().splitlines()[0][:100]
    for i, t in enumerate(tests):
        try:
            rp = subprocess.run([exe], input=t.get("input", ""), capture_output=True,
                                text=True, encoding="utf-8", errors="replace", timeout=5)
        except subprocess.TimeoutExpired:
            return f"case{i}:timeout"
        if rp.stdout.strip() != str(t.get("expected", "")).strip():
            return f"case{i} expected {t.get('expected')!r} got {rp.stdout.strip()!r}"
    return None


def fill_code(q, answers):
    tpl = (q.get("body") or {}).get("template") or ""
    return BLANK_RE.sub(lambda m: str(answers[ord(m.group(1)) - 0x2460]), tpl)


def blank_values(q):
    """fill 答案归一化：原生格式是 [{"blank":1,"answer":"/"}]，v2 是 ["/","%"]。"""
    raw = (q.get("answer") or {}).get("answer") or []
    out = []
    for i, a in enumerate(raw):
        if isinstance(a, dict):
            out.append(str(a.get("answer", "")))
        else:
            out.append(str(a))
    return out


def blank_audit(banks, vm, ctx, only_ids):
    cc = getattr(vm, "CC", None)
    if cc is None:
        ctx.err("V8 无法执行：找不到 C 编译器（CC=None），判题材料未经实跑")
        return
    pre = getattr(vm, "PRE", "#include <stdio.h>\n#include <string.h>\n#include <stdlib.h>\n")
    work = tempfile.mkdtemp(prefix="qc_blank_")
    try:
        for bank, d in banks.items():
            for q in d.get("questions") or []:
                if qtype(q) != "fill":
                    continue
                qid = str(q.get("id"))
                if only_ids and qid not in only_ids:
                    continue
                answers = blank_values(q)
                harness = (q.get("judge") or {}).get("harness") or ""
                tests = (q.get("judge") or {}).get("tests") or []
                if not harness or len(tests) < 1 or not answers:
                    continue
                official = pre + fill_code(q, answers) + "\n" + harness + "\n"
                msg = run_all(cc, work, f"o_{qid}", official, tests)
                if msg:
                    ctx.err(f"{bank} {qid}: 官方答案回放失败 {msg}")
                    continue
                for idx in range(len(answers)):
                    ans = str(answers[idx])
                    cands = list(dict.fromkeys(CAND.get(ans, [])))
                    if re.fullmatch(r"-?\d+", ans):
                        cands += [str(int(ans) + 1), str(int(ans) - 1)]
                    for cand in cands:
                        if cand == ans:
                            continue
                        trial = list(answers)
                        trial[idx] = cand
                        code = pre + fill_code(q, trial) + "\n" + harness + "\n"
                        out = run_all(cc, work, f"m_{qid}_{idx}", code, tests)
                        if out is None:
                            where = f"{bank} {qid}: 第{idx + 1}空填 {cand!r}（官方 {ans!r}）仍全过"
                            # 仅凭“仍全过”无法区分“域内等价”与“真漏判”（例如 max 题里 > 与 >=、
                            # 2*x+1 里 + 与 |、小写转大写里 -32 与 ^32 都输出等价），
                            # 因此统一记 REVIEW，由复核人逐条裁决；判为漏判必须补用例或改题。
                            ctx.review(where + " → 疑似未钉死或域内等价：判为漏判则补用例/改题，"
                                               "判为等价则记遗留项")
    finally:
        for f in os.listdir(work):
            try:
                os.remove(os.path.join(work, f))
            except OSError:
                pass
        try:
            os.rmdir(work)
        except OSError:
            pass


# ------------------------------------------------------------------ 报告

def stats(banks):
    rows = []
    for bank, d in banks.items():
        qs = d.get("questions") or []
        diff = Counter(q.get("difficulty") for q in qs)
        singles = [q for q in qs if qtype(q) == "single"]
        letters = Counter((q.get("answer") or {}).get("answer") for q in singles)
        cyc = sum(1 for i, q in enumerate(singles)
                  if (q.get("answer") or {}).get("answer") == "ABCD"[i % 4])
        fam, big = check_skeletons(bank, qs, Ctx())
        code_n = sum(1 for q in qs if code_blocks(q.get("stem")))
        rows.append({
            "bank": bank, "n": len(qs), "diff": dict(sorted(diff.items(), key=lambda kv: str(kv[0]))),
            "total": sum(q.get("score") or 0 for q in qs),
            "multi": sum(1 for q in qs if qtype(q) == "multiple"),
            "letters": {k: letters.get(k, 0) for k in "ABCD"} if singles else {},
            "cyc": f"{cyc}/{len(singles)}" if singles else "-",
            "skel": fam, "maxfam": big, "code": code_n,
            "kp": len({q.get("kp") for q in qs if q.get("kp")}),
        })
    return rows


def write_report(path, root, rows, ctx, gate):
    lines = ["# 题库质检报告（question-bank-qc）", "",
             f"- 对象：`{root}`", f"- 门禁：{gate}",
             f"- 结论：**{'PASS（可交付）' if not ctx.errors else 'FAIL（有 error）'}**",
             f"- counts：errors={len(ctx.errors)} warnings={len(ctx.warns)} review={len(ctx.reviews)}", "",
             "## 1. 统计", "",
             "| 库 | 题数 | difficulty | 总分 | 多选 | A/B/C/D | ABCD 循环 | 骨架种类 | 最大族 | 含代码题 | kp 数 |",
             "|---|---|---|---|---|---|---|---|---|---|---|"]
    for r in rows:
        row = dict(r)
        row["letters"] = "/".join(str(r["letters"].get(k, 0)) for k in "ABCD") if r["letters"] else "-"
        lines.append("| {bank} | {n} | {diff} | {total} | {multi} | {letters} | {cyc} | {skel} | {maxfam} | {code} | {kp} |".format(**row))
    for title, items, tag in (("2. 必改项（error）", ctx.errors, "E"),
                              ("3. 需处理（warning）", ctx.warns, "W"),
                              ("4. 待人工裁决（REVIEW）", ctx.reviews, "R")):
        lines += ["", f"## {title}", ""]
        lines += [f"- [{tag}{i + 1}] {m}" for i, m in enumerate(items)] or ["- 无"]
    lines += ["", "## 5. 复现", "", "```bash", f"python qc.py {root}", "```", ""]
    with io.open(path, "w", encoding="utf-8") as f:
        f.write("\n".join(lines))


def load_verify(path, ctx):
    spec = importlib.util.spec_from_file_location("qc_verify_mod", path)
    vm = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(vm)
    return vm


def find_verify(explicit, root):
    cands = []
    if explicit:
        cands.append(explicit)
    here = os.path.dirname(os.path.abspath(__file__))
    cands += [os.path.join(here, "verify.py"), os.path.join(root, "verify.py"),
              os.path.join(here, "..", "..", "verify.py")]
    for c in cands:
        if c and os.path.exists(c):
            return os.path.abspath(c)
    return None


def _utf8_stdout() -> None:
    try:
        sys.stdout.reconfigure(encoding="utf-8")
        sys.stderr.reconfigure(encoding="utf-8")
    except Exception:
        pass


def main():
    _utf8_stdout()
    ap = argparse.ArgumentParser(description="题库质检补充门禁")
    ap.add_argument("root")
    ap.add_argument("--no-compile", action="store_true", help="跳过判题实跑（只做结构检查，不是完整门禁）")
    ap.add_argument("--blank-audit", action="store_true", help="逐空注入错误值，检查是否被测试钉死")
    ap.add_argument("--strict-kp", action="store_true", help="越纲 kp 计为 error")
    ap.add_argument("--report", help="输出 markdown 报告")
    ap.add_argument("--verify", help="verify.py 路径")
    ap.add_argument("--ids", help="只审计这些题号（逗号分隔）")
    ap.add_argument("--no-gate", action="store_true",
                    help="不重复调用 verify.py（仅当同一轮已跑过门禁时用；不能单独作为交付依据）")
    args = ap.parse_args()

    root = os.path.abspath(args.root)
    if not os.path.isdir(root):
        print(f"目录不存在：{root}")
        return 2
    ctx = Ctx()
    verify_path = find_verify(args.verify, root)
    vm = None
    gate_line = "未运行"
    std_present = [fn for fn, _n in STD_FILES.values() if os.path.exists(os.path.join(root, fn))]
    if not verify_path:
        ctx.err("找不到 verify.py（用 --verify 指定）")
    else:
        try:
            vm = load_verify(verify_path, ctx)
        except Exception as e:  # noqa: BLE001
            ctx.warn(f"无法导入 verify.py（{e}），跳过依赖它的检查")
    if verify_path and args.no_gate:
        gate_line = "未运行（--no-gate）"
        ctx.warn("使用了 --no-gate：本轮未跑 verify.py，不能单独作为交付依据")
    elif verify_path and not std_present:
        ctx.warn("目录里没有 verify.py 约定的 5 个原生文件名，verify.py 当不了门禁："
                 "分离格式（format_version=2）请先 convert_pair/load_paths 转成原生 JSON，再跑 verify.py")
    elif verify_path:
        cmd = [sys.executable, verify_path, root] + (["--no-compile"] if args.no_compile else [])
        print("== 门禁 ==")
        print("$ " + " ".join(cmd))
        tmp = tempfile.mkdtemp(prefix="qc_gate_")
        cp = subprocess.run(cmd, cwd=tmp, capture_output=True, text=True,
                            encoding="utf-8", errors="replace")
        print((cp.stdout or "").strip())
        if cp.stderr:
            print((cp.stderr or "").strip()[:800])
        try:
            os.rmdir(tmp)
        except OSError:
            pass
        m = re.search(r"errors=(\d+) warnings=(\d+)", cp.stdout or "")
        gate_line = f"errors={m.group(1)} warnings={m.group(2)}（exit {cp.returncode}）" if m else f"exit {cp.returncode}"
        if cp.returncode != 0:
            ctx.err(f"verify.py 未通过：{gate_line}")
        if args.no_compile:
            ctx.warn("使用了 --no-compile：fill/fix 判题材料未实跑，本次不是完整门禁")
        if vm is not None and getattr(vm, "CC", None) is None and not args.no_compile:
            ctx.err("找不到 C 编译器：verify.py 会静默跳过判题实跑（假 PASS），先装 clang/gcc")

    print("== 补充检查 ==")
    banks = load_banks(root, ctx)
    if not banks:
        ctx.err(f"{root} 下没有可识别的题库文件")
    for bank, d in banks.items():
        qs = d.get("questions") or []
        check_options(bank, qs, ctx)
        check_typography(bank, qs, ctx)
        check_skeletons(bank, qs, ctx)
        check_difficulty(bank, qs, ctx)
    check_kp(banks, ctx, args.strict_kp)
    check_ids(banks, ctx)
    check_judge_io(banks, ctx)
    if args.blank_audit and vm is not None and not args.no_compile:
        ids = {x.strip() for x in (args.ids or "").split(",") if x.strip()}
        blank_audit(banks, vm, ctx, ids)

    for e in ctx.errors:
        print("  ERROR", e)
    for w in ctx.warns:
        print("  WARN ", w)
    for r in ctx.reviews:
        print("  REVIEW", r)
    print(f"errors={len(ctx.errors)} warnings={len(ctx.warns)} review={len(ctx.reviews)}")
    print("PASS" if not ctx.errors else "FAIL")

    if args.report:
        write_report(args.report, root, stats(banks), ctx, gate_line)
        print(f"报告已写入 {args.report}")
    return 0 if not ctx.errors else 1


if __name__ == "__main__":
    sys.exit(main())
