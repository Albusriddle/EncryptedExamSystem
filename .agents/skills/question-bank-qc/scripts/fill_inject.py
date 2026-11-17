# -*- coding: utf-8 -*-
"""填空题空位审查：官方答案回填必须全过 + 逐空注入候选错误值，看能否被用例抓住。

对每个空生成同类候选（比较/算术/逻辑/位运算互替、数值 ±1），只替换该空、其余用
官方答案，重跑 judge.tests。若某候选仍全部通过 => 该空"未被钉死"（UNPINNED）。

用法:
  python fill_inject.py <题库目录> [--ids F-0001,F-0006] [--per-blank 4] [--jobs 8]
                        [--strict]   # --strict 时 UNPINNED 也返回 1
输出: baseline 失败、UNPINNED 明细、统计；退出码 0/1
"""
import io, json, os, re, shutil, subprocess, sys, tempfile
from concurrent.futures import ThreadPoolExecutor

sys.stdout.reconfigure(encoding="utf-8", errors="replace")

CC = None
for cand in (r"C:\msys64\mingw64\bin\clang.exe", r"C:\msys64\mingw64\bin\gcc.exe"):
    if os.path.exists(cand):
        CC = cand
        break
if CC is None:
    from shutil import which
    CC = which("clang") or which("gcc")
if CC is None:
    print("找不到 C 编译器"); sys.exit(2)

PRE = "#include <stdio.h>\n#include <string.h>\n#include <stdlib.h>\n"
CMP = ["<", ">", "<=", ">=", "==", "!="]
ARI = ["+", "-", "*", "/", "%"]
LOG = ["&&", "||", "&", "|"]
BIT = ["&", "|", "^"]
MARKS = "①②③④⑤"


KW = {"int", "void", "if", "else", "while", "for", "return", "char", "double",
      "float", "long", "unsigned", "const", "static", "case", "default", "switch"}
TYPE = ["int", "char", "short", "long", "float", "double", "unsigned"]
SYN = {"sizeof": ["strlen"], "strlen": ["sizeof"], "double": TYPE, "float": TYPE,
       "int": TYPE, "char": TYPE, "long": TYPE, "short": TYPE, "unsigned": TYPE}


def candidates(ans, tpl=""):
    a = (ans or "").strip()
    if re.fullmatch(r"[-+*/%<>=!&|^]{1,2}", a):
        pool = CMP if a in CMP else ARI if a in ARI else LOG if a in LOG else BIT if a in BIT else CMP + ARI
        if a in ("+", "-", "&", "|", "^"):
            pool = pool + BIT
        return [c for c in pool if c != a]
    if re.fullmatch(r"-?\d+", a):
        v = int(a)
        return [str(x) for x in (v - 1, v + 1, 0) if x != v]
    m = re.fullmatch(r'"?%[-0-9.]*[a-z]"?', a)                # 格式串（含/不含引号）
    if m:
        bare = a.strip('"')
        return [f'"{s}"' if a.startswith('"') else s
                for s in ("%f", "%c", "%10d", "%-5d", "%s") if s != bare][:3]
    if a in SYN:                                             # 类型/函数名互替
        return [x for x in SYN[a] if x != a][:3]
    m = re.fullmatch(r"(\w+)\s*([-+*/%])\s*(\w+)", a)      # 形如 a / b 的表达式：只换运算符
    if m:
        return [f"{m.group(1)} {op} {m.group(3)}" for op in ARI if op != m.group(2)][:4]
    if re.fullmatch(r"[A-Za-z_]\w{0,2}", a):                 # 单个标识符：换成本模板里的其它标识符
        seen = [x for x in re.findall(r"[A-Za-z_]\w{0,2}", tpl)
                if x != a and x not in KW]
        out = []
        for x in seen:
            if x not in out:
                out.append(x)
        return out[:3]
    return []


def fill_code(q, override=None):
    tpl = (q.get("body") or {}).get("template") or ""
    blanks = (q.get("answer") or {}).get("answer") or []
    for i, b in enumerate(blanks):
        val = str(b.get("answer"))
        if override and override[0] == i:
            val = override[1]
        tpl = tpl.replace(f"____{MARKS[i]}____", val)
    return tpl


def run_tests(code, tests, harness):
    work = tempfile.mkdtemp(prefix="qc_fill_")
    try:
        src = os.path.join(work, "q.c")
        exe = os.path.join(work, "q.exe")
        io.open(src, "w", encoding="utf-8").write(PRE + code + "\n" + harness + "\n")
        cp = subprocess.run([CC, "-O0", "-std=c11", "-w", "-o", exe, src],
                            capture_output=True, text=True, encoding="utf-8", errors="replace")
        if cp.returncode:
            return False, "compile_error"
        for i, t in enumerate(tests):
            try:
                rp = subprocess.run([exe], input=str(t.get("input", "")),
                                    capture_output=True, text=True, encoding="utf-8",
                                    errors="replace", timeout=5, cwd=work)
            except subprocess.TimeoutExpired:
                return False, f"timeout case{i}"
            if rp.stdout.strip() != str(t.get("expected", "")).strip():
                return False, f"case{i} got {rp.stdout.strip()!r}"
        return True, None
    finally:
        shutil.rmtree(work, ignore_errors=True)


def main():
    root = sys.argv[1] if len(sys.argv) > 1 else "."
    per = int(sys.argv[sys.argv.index("--per-blank") + 1]) if "--per-blank" in sys.argv else 4
    jobs = int(sys.argv[sys.argv.index("--jobs") + 1]) if "--jobs" in sys.argv else 8
    d = json.load(io.open(os.path.join(root, "fill.json"), encoding="utf-8"))
    qs = d["questions"]
    if "--ids" in sys.argv:
        want = set(sys.argv[sys.argv.index("--ids") + 1].split(","))
        qs = [q for q in qs if q["id"] in want]

    baseline_fail, unpinned, skipped, pinned = [], [], [], 0
    tasks = []                                   # (q, i, official, cand)
    for q in qs:
        tests = (q.get("judge") or {}).get("tests") or []
        harness = (q.get("judge") or {}).get("harness") or ""
        ok, why = run_tests(fill_code(q), tests, harness)
        if not ok:
            baseline_fail.append((q["id"], why))
            continue
        for i, b in enumerate((q.get("answer") or {}).get("answer") or []):
            cands = candidates(b.get("answer"), (q.get("body") or {}).get("template") or "")[:per]
            if not cands:
                skipped.append((q["id"], i + 1, b.get("answer")))
            for c in cands:
                tasks.append((q, i, str(b.get("answer")), c, tests, harness))

    def work(t):
        q, i, official, cand, tests, harness = t
        ok, _ = run_tests(fill_code(q, (i, cand)), tests, harness)
        return (q["id"], i + 1, official, cand, ok)

    with ThreadPoolExecutor(max_workers=jobs) as pool:
        for qid, no, official, cand, still_pass in pool.map(work, tasks):
            if still_pass:
                unpinned.append((qid, no, official, cand))
            else:
                pinned += 1

    grouped = {}
    for qid, no, official, cand in unpinned:
        grouped.setdefault((qid, no, official), []).append(cand)

    print(f"fill={len(qs)} baseline_fail={len(baseline_fail)} "
          f"inject={len(tasks)} caught={pinned} unpinned_blank={len(grouped)} skipped={len(skipped)}")
    for x in baseline_fail:
        print("  BASELINE_FAIL", x)
    for (qid, no, official), cands in grouped.items():
        print(f"  UNPINNED {qid} blank{no} official={official!r} 也可通过={cands}")
    for x in skipped[:20]:
        print(f"  SKIP {x[0]} blank{x[1]} answer={x[2]!r}（无同类候选，需人工判断）")
    bad = len(baseline_fail) or (len(grouped) and "--strict" in sys.argv)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
