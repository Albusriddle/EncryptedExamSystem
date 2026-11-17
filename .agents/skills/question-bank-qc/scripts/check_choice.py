# -*- coding: utf-8 -*-
"""选择题代码块批量编译实跑：题干 ```c 代码 → 编译 → 运行 → stdout 与选项逐字比对。

判定：stdout 去首尾空白后命中的选项集合 hits；单选要求 hits == {answer} 且唯一；
多选要求 set(hits) == set(answer)。未命中 / 多命中 / 编译失败 / 需 stdin 均单列。

用法:
  python check_choice.py <题库目录> --all [--jobs 8] [--stdin-file in.txt]
  python check_choice.py <题库目录> --sample 21 --seed 20261117
  python check_choice.py <题库目录> --ids C-0001,C-0167
输出: stats {...} + 失败明细；退出码 0 = 全部命中, 1 = 有失败
"""
import io, json, os, re, shutil, subprocess, sys, tempfile, random
from concurrent.futures import ThreadPoolExecutor

sys.stdout.reconfigure(encoding="utf-8", errors="replace")

CC = None
for cand in (r"C:\msys64\mingw64\bin\clang.exe", r"C:\msys64\mingw64\bin\gcc.exe"):
    if os.path.exists(cand):
        CC = cand
        break
if CC is None:                                  # Linux/macOS 或 PATH 里已有编译器
    from shutil import which
    CC = which("clang") or which("gcc")
if CC is None:
    print("找不到 C 编译器：请设置 CC 或安装 clang/gcc")
    sys.exit(2)
PRE = "#include <stdio.h>\n#include <string.h>\n#include <stdlib.h>\n"
FENCE = re.compile(r"```(?:c)?\n?(.*?)```", re.S)


def load(root):
    return json.load(io.open(os.path.join(root, "choice.json"), encoding="utf-8"))


def visible(s):
    for ch in ("\u00a0", "\u3000", "\u200b", "\t", "\r"):
        s = s.replace(ch, " ")
    return re.sub(r" +", " ", s).strip()


def run_one(q, stdin_text, keep_dir=None):
    stem = q.get("stem") or ""
    blocks = [b for b in FENCE.findall(stem) if b.strip()]
    if not blocks:
        return {"id": q["id"], "state": "no-code"}
    block = blocks[0].strip("\n")
    if "int main" not in block:
        return {"id": q["id"], "state": "no-main"}
    work = keep_dir or tempfile.mkdtemp(prefix="qc_choice_")
    try:
        return _compile_and_run(q, block, stdin_text, work)
    finally:
        if keep_dir is None:
            shutil.rmtree(work, ignore_errors=True)


def _compile_and_run(q, block, stdin_text, work):
    src = os.path.join(work, "q.c")
    exe = os.path.join(work, "q.exe")
    io.open(src, "w", encoding="utf-8").write(PRE + block + "\n")
    cp = subprocess.run([CC, "-O0", "-std=c11", "-w", "-o", exe, src],
                        capture_output=True, text=True, encoding="utf-8", errors="replace")
    if cp.returncode:
        return {"id": q["id"], "state": "compile-error",
                "detail": (cp.stderr or "")[:200].replace("\n", " ")}
    try:
        rp = subprocess.run([exe], input=stdin_text, capture_output=True, text=True,
                            encoding="utf-8", errors="replace", timeout=5, cwd=work)
    except subprocess.TimeoutExpired:
        return {"id": q["id"], "state": "timeout"}
    out = rp.stdout.strip()
    opts = (q.get("body") or {}).get("options") or []
    hits = [o["key"] for o in opts if (o.get("text") or "").strip() == out]
    if not hits:
        hits = [o["key"] for o in opts if visible(o.get("text") or "") == visible(out)]
    ans = (q.get("answer") or {}).get("answer")
    want = set(ans) if isinstance(ans, list) else {ans}
    ok = set(hits) == want
    return {"id": q["id"], "state": "ok" if ok else "bad", "out": out,
            "hits": hits, "ans": ans, "type": q.get("type")}


def main():
    root = sys.argv[1] if len(sys.argv) > 1 else "."
    stdin_text = ""
    if "--stdin-file" in sys.argv:
        stdin_text = io.open(sys.argv[sys.argv.index("--stdin-file") + 1],
                             encoding="utf-8").read()
    jobs = int(sys.argv[sys.argv.index("--jobs") + 1]) if "--jobs" in sys.argv else 8
    d = load(root)
    qs = d.get("questions") or []
    if "--ids" in sys.argv:
        want = set(sys.argv[sys.argv.index("--ids") + 1].split(","))
        qs = [q for q in qs if q["id"] in want]
    elif "--sample" in sys.argv:
        n = int(sys.argv[sys.argv.index("--sample") + 1])
        seed = int(sys.argv[sys.argv.index("--seed") + 1]) if "--seed" in sys.argv else 0
        rnd = random.Random(seed)
        singles = [q for q in qs if q.get("type") == "single"]
        multis = [q for q in qs if q.get("type") == "multiple"]
        per = max(1, n // 3)
        picked = []
        for lvl in (1, 2, 3):
            pool = [q for q in singles if q.get("difficulty") == lvl]
            picked += rnd.sample(pool, min(per, len(pool)))
        picked += rnd.sample(multis, min(n - len(picked), len(multis)))
        qs = picked
    elif "--all" not in sys.argv:
        print("用法: --all | --sample N --seed S | --ids a,b,c"); return 2

    with ThreadPoolExecutor(max_workers=jobs) as pool:
        results = list(pool.map(lambda q: run_one(q, stdin_text), qs))
    stat = {}
    for r in results:
        stat[r["state"]] = stat.get(r["state"], 0) + 1
    stat["checked"] = len(results)
    print("stats", json.dumps(stat, ensure_ascii=False, sort_keys=True))
    for r in results:
        if r["state"] in ("bad", "compile-error", "timeout", "ambiguous"):
            print(f"  {r['state']:>13} {r['id']} out={r.get('out')!r} "
                  f"hits={r.get('hits')} ans={r.get('ans')} {r.get('detail','')}")
    for r in results:
        if r["state"] == "ok" and "--show-ok" in sys.argv:
            print(f"  ok {r['id']} out={r['out']!r} hits={r['hits']}")
    manual = [r["id"] for r in results if r["state"] in ("no-main", "no-code")]
    if manual:
        print(f"未实跑（无 main / 无代码）：{len(manual)} 题 -> 人工核对：{manual[:20]}")
    return 0 if stat.get("bad", 0) == 0 and stat.get("compile-error", 0) == 0 \
        and stat.get("timeout", 0) == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
