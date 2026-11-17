# -*- coding: utf-8 -*-
"""题库门禁：结构/配额/答案位置/重复/跨题引用/判题材料实跑。

用法： python verify.py <题库目录> [--no-compile]
通过： 退出码 0，最后一行 PASS；否则 FAIL 并列出错误。
"""
import json, io, os, re, sys, subprocess, hashlib, tempfile
from collections import Counter, defaultdict

CC = None
for cand in (r"C:\msys64\mingw64\bin\clang.exe", r"C:\msys64\mingw64\bin\gcc.exe"):
    if os.path.exists(cand):
        CC = cand
        break

FILES = {
    "choice": ("choice.json", 500),
    "fill": ("fill.json", 50),
    "fix": ("fix.json", 50),
    "rutuan": ("rutuan-q16-19.json", 4),
    "zhaosheng": ("zhaosheng-b-q16-19.json", 4),
}
KPS = {
    "1.1","1.2","1.3","1.4","1.5","2.1","2.2","2.3","2.4","2.5","2.6",
    "3.1","3.2","3.3","4.1","4.2","4.3","4.4","5.1","5.2","5.3","5.4","5.5",
    "6.1","6.2","6.3","6.4","6.5","6.6","6.7","7.1","7.2","7.3","7.4",
    "8.1","8.2","8.3","8.4","9.1","9.2","9.3","10.1","10.2","10.3",
    "11.1","11.2","11.3","11.4","11.5","12.1","12.2",
}
CROSS = re.compile(r"上题|上一题|上述|第\s*\d+\s*题|见图|见表|沿用|同上")

errors, warnings = [], []
def err(msg): errors.append(msg)
def warn(msg): warnings.append(msg)

def load(path):
    return json.load(io.open(path, encoding="utf-8"))

def norm_stem(s):
    s = re.sub(r"```.*?```", "<CODE>", s or "", flags=re.S)
    s = re.sub(r"`[^`]*`", "<C>", s)
    s = re.sub(r"\d+", "<N>", s)
    return re.sub(r"\s+", "", s)

def opt_list(q):
    opts = (q.get("body") or {}).get("options") or []
    return [o.get("key") for o in opts], [o.get("text") for o in opts]

def check_question(q, bank, idx):
    where = f"{bank}[{idx}] {q.get('id')}"
    if not q.get("id"): err(f"{where}: 缺少 id")
    t = q.get("type")
    if t not in ("single","multiple","judge","fill","fix","essay","code"):
        err(f"{where}: 非法 type={t!r}")
    if not (q.get("stem") or "").strip(): err(f"{where}: 题干为空")
    kp = q.get("kp")
    if not (isinstance(kp, str) and re.fullmatch(r"(?:[1-9]|1[0-2])\.[1-9]", kp)):
        err(f"{where}: kp={kp!r} 不在 1.1–12.9")
    elif kp not in KPS:
        warn(f"{where}: kp={kp!r} 不在原 51 点大纲内")
    d = q.get("difficulty")
    if t == "code":
        allowed_d = (1,2,3,4)
    elif t == "multiple":
        allowed_d = (1,2,3,9)      # 9 = 多选题统一难度（本项目口径）
    else:
        allowed_d = (1,2,3)
    if d not in allowed_d: err(f"{where}: difficulty={d!r} 不合法（允许 {allowed_d}）")
    if not isinstance(q.get("score"), int): err(f"{where}: score 必须是整数")
    stem = q.get("stem","")
    for blk in re.findall(r"```(?:c)?\n?(.*?)```", stem, re.S):
        body = blk.strip("\n")
        lines = body.splitlines()
        if not body.strip():
            err(f"{where}: 空代码块")
        elif "int main" in body and len(lines) == 1:
            err(f"{where}: 完整程序被压成单行，必须多行缩进")
        elif len(lines) == 1 and len(body) > 40 and ";" in body:
            err(f"{where}: 单行代码过长（{len(body)} 字符），必须换行")
    no_fence = re.sub(r"```(?:c)?\n?.*?```", "", stem, flags=re.S)
    for inline in re.findall(r"`([^`]*)`", no_fence):
        if "int main" in inline or (len(inline) > 60 and ";" in inline and "{" in inline):
            err(f"{where}: 程序片段用了行内代码，必须用多行围栏块")
    if CROSS.search(json.dumps(q, ensure_ascii=False)):
        # 只对题干判，判题材料里允许出现“第 N 行”
        if CROSS.search(q.get("stem","")):
            err(f"{where}: 题干含跨题引用")
    body = q.get("body") or {}
    kind = body.get("kind")
    if t in ("single","multiple","judge"):
        if kind != "options": err(f"{where}: 选择题 body.kind 必须 options")
        keys, texts = opt_list(q)
        if len(keys) != 4: err(f"{where}: 选项必须 4 个")
        if len(set(keys)) != len(keys): err(f"{where}: 选项键重复")
        if len(set(texts)) != len(texts): err(f"{where}: 选项文本重复")
        ans = (q.get("answer") or {})
        ak = ans.get("kind")
        a = ans.get("answer")
        if t == "multiple":
            if ak != "multi": err(f"{where}: 多选 answer.kind 必须 multi")
            if not isinstance(a, list) or not (2 <= len(a) <= 3): err(f"{where}: 多选答案须 2–3 项")
            elif sorted(a) != a: err(f"{where}: 多选答案未升序")
            elif any(x not in keys for x in a): err(f"{where}: 多选答案不在选项内")
        else:
            if ak != "single": err(f"{where}: 单选 answer.kind 必须 single")
            if a not in keys: err(f"{where}: 答案 {a!r} 不在选项内")
    elif t == "fill":
        if kind != "template": err(f"{where}: fill body.kind 必须 template")
        tpl = body.get("template") or ""
        n = body.get("blank_count")
        marks = len(re.findall(r"____[①②③④⑤]____", tpl))
        if n != 3 or marks != 3: err(f"{where}: 必须 3 个占位符/blank_count=3（实际 {marks}/{n}）")
        ans = (q.get("answer") or {})
        if ans.get("kind") != "blanks": err(f"{where}: fill answer.kind 必须 blanks")
        if len(ans.get("answer") or []) != 3: err(f"{where}: fill 答案须 3 项")
    elif t == "fix":
        if kind != "code": err(f"{where}: fix body.kind 必须 code")
        code = body.get("code") or []
        n = body.get("blank_count")
        if n != 2: err(f"{where}: fix blank_count 必须 2")
        ans = (q.get("answer") or {})
        if ans.get("kind") != "fixes": err(f"{where}: fix answer.kind 必须 fixes")
        fixes = ans.get("answer") or []
        if len(fixes) != 2: err(f"{where}: fix 必须有 2 处修改")
        for f in fixes:
            ln = f.get("line")
            if not isinstance(ln, int) or not (1 <= ln <= len(code)):
                err(f"{where}: line={ln} 越界（共 {len(code)} 行）"); continue
            if f.get("wrong") != code[ln-1]:
                err(f"{where}: 第 {ln} 行 wrong 与原文不一致")
            if not isinstance(f.get("answer"), str): err(f"{where}: 修正答案必须是字符串")
    elif t == "code":
        if kind != "text": err(f"{where}: 代码题 body.kind 必须 text")
        ans = q.get("answer") or {}
        if ans.get("kind") != "rubric": err(f"{where}: 大题 answer.kind 必须 rubric")
        if ans.get("total") != q.get("score"): err(f"{where}: rubric.total != score")
        pts = ans.get("points") or ans.get("key_points") or []
        s = sum(p.get("score", 0) for p in pts if isinstance(p, dict))
        if pts and s != q.get("score"): err(f"{where}: rubric 评分点合计 {s} != {q.get('score')}")

def compile_run(code, inp, tag, work):
    src = os.path.join(work, tag + ".c"); exe = os.path.join(work, tag + ".exe")
    io.open(src, "w", encoding="utf-8").write(code)
    cp = subprocess.run([CC, "-O0", "-std=c11", "-w", "-o", exe, src],
                        capture_output=True, text=True, encoding="utf-8", errors="replace")
    if cp.returncode != 0:
        return None, "compile_error:" + (cp.stderr or "")[:120]
    try:
        rp = subprocess.run([exe], input=inp, capture_output=True, text=True,
                            encoding="utf-8", errors="replace", timeout=5)
        return rp.stdout.strip(), None
    except subprocess.TimeoutExpired:
        return None, "timeout"

PRE = "#include <stdio.h>\n#include <string.h>\n#include <stdlib.h>\n"

def run_tests(code, tests, tag, work):
    for i, t in enumerate(tests):
        got, e = compile_run(code, t.get("input",""), f"{tag}_{i}", work)
        if e: return e
        if got != str(t.get("expected","")).strip():
            return f"case{i} expected {t.get('expected')!r} got {got!r}"
    return None

def check_judge(bank, q, idx, work):
    where = f"{bank}[{idx}] {q.get('id')}"
    j = q.get("judge") or {}
    tests = j.get("tests") or []
    harness = j.get("harness") or ""
    if len(tests) < 3: err(f"{where}: judge.tests 少于 3 条")
    if not harness.strip(): err(f"{where}: 缺少 judge.harness"); return
    if CC is None: warnings.append("未找到编译器，跳过判题实跑"); return
    body = q.get("body") or {}
    if q.get("type") == "fill":
        tpl = body.get("template","")
        ans = [a.get("answer") for a in (q.get("answer") or {}).get("answer",[])]
        code = tpl
        for i,a in enumerate(ans): code = code.replace(f"____{chr(0x2460+i)}____", str(a))
        msg = run_tests(PRE+code+"\n"+harness+"\n", tests, "f_"+str(q["id"]), work)
        if msg: err(f"{where}: fill 官方答案回放失败 {msg}")
    else:
        code_lines = list(body.get("code") or [])
        fixes = (q.get("answer") or {}).get("answer",[])
        fixed = list(code_lines)
        for f in fixes: fixed[int(f["line"])-1] = f["answer"]
        msg = run_tests(PRE+"\n".join(fixed)+"\n"+harness+"\n", tests, "x_"+str(q["id"]), work)
        if msg: err(f"{where}: fix 官方答案回放失败 {msg}")
        msg = run_tests(PRE+"\n".join(code_lines)+"\n"+harness+"\n", tests, "o_"+str(q["id"]), work)
        if msg is None: err(f"{where}: fix 原始代码不应通过测试")
        for i in range(len(fixes)):
            sub = [f for j,f in enumerate(fixes) if j != i]
            c = list(code_lines)
            for f in sub: c[int(f["line"])-1] = f["answer"]
            msg = run_tests(PRE+"\n".join(c)+"\n"+harness+"\n", tests, f"m_{q['id']}_{i}", work)
            if msg is None: err(f"{where}: 撤销第 {i+1} 处修改后仍通过（该处不可测）")

def check_bank(bank, path, expect_n):
    d = load(path)
    qs = d.get("questions") or []
    if not d.get("exam_id"): err(f"{bank}: 缺少 exam_id")
    if len(qs) != expect_n: err(f"{bank}: 题数 {len(qs)} != {expect_n}")
    pick = d.get("pick")
    if not isinstance(pick, int) or not (1 <= pick <= max(1, len(qs))):
        err(f"{bank}: pick={pick!r} 必须在 1..{len(qs)}")
    ids = [q.get("id") for q in qs]
    if len(ids) != len(set(ids)): err(f"{bank}: id 重复")
    for i,q in enumerate(qs): check_question(q, bank, i)
    # 难度配额
    if bank == "choice":
        c = Counter(q.get("difficulty") for q in qs if q.get("type") == "single")
    else:
        c = Counter(q.get("difficulty") for q in qs if q.get("type") in ("single","multiple","judge","fill","fix"))
    if bank in ("choice","fill","fix"):
        for lvl in (1,2,3):
            if c.get(lvl,0) == 0: err(f"{bank}: 难度 {lvl} 为 0")
        if c and max(c.values()) - min(c.values()) > 2:
            err(f"{bank}: 难度配额不均衡 {dict(c)}")
    # 重复：全文（仅去空白）不得重复；骨架族不得过大
    full = [re.sub(r"\s+", "", q.get("stem","")) for q in qs]
    dup = [s for s,n in Counter(full).items() if n > 1]
    if dup: err(f"{bank}: 题干逐字重复 {len(dup)} 组")
    fam = Counter(norm_stem(q.get("stem","")) for q in qs)
    if fam and max(fam.values()) > 4:
        err(f"{bank}: 骨架族过大（最大 {max(fam.values())} 题，上限 4）")
    need = 120 if bank == "choice" else 30
    if len(fam) < min(need, len(qs)):
        err(f"{bank}: 骨架种类过少（{len(fam)}，下限 {min(need,len(qs))}）")
    # 答案位置（单选）
    letters = Counter()
    for q in qs:
        if q.get("type") == "single":
            a = (q.get("answer") or {}).get("answer")
            letters[a] += 1
    if bank == "choice":
        total = sum(letters.values())
        if total:
            for k in "ABCD":
                if not (total*0.18 <= letters.get(k,0) <= total*0.32):
                    err(f"choice: 答案字母 {k} 数量 {letters.get(k,0)} 超出均衡范围")
            cyc = sum(1 for i,q in enumerate([x for x in qs if x.get("type")=="single"])
                      if (q.get("answer") or {}).get("answer") == "ABCD"[i%4])
            if cyc / total > 0.35:
                err(f"choice: 答案疑似 ABCD 循环（吻合 {cyc}/{total}）")
    return d, qs

def cross_bank_check(loaded):
    """跨库逐字题干、跨库同模板/同代码体。"""
    for a in ("choice","fill","fix"):
        if a not in loaded: continue
        for b in ("choice","fill","fix"):
            if b <= a or b not in loaded: continue
            sa = {re.sub(r"\s+","",q.get("stem","")) for q in loaded[a]}
            sb = {re.sub(r"\s+","",q.get("stem","")) for q in loaded[b]}
            same = sa & sb
            if same: err(f"{a}/{b}: 跨库逐字同题干 {len(same)} 组")
    # 判题材料写当前目录（并发/只读风险）
    for bank in ("fill","fix"):
        for q in loaded.get(bank, []):
            h = (q.get("judge") or {}).get("harness") or ""
            if re.search(r"fopen|fprintf|freopen", h):
                err(f"{bank} {q.get('id')}: harness 含文件写入，判题/并发不安全")

def main():
    root = sys.argv[1] if len(sys.argv) > 1 else "."
    no_compile = "--no-compile" in sys.argv
    work = tempfile.mkdtemp(prefix="qcbank-verify-")
    loaded = {}
    for bank,(name,n) in FILES.items():
        p = os.path.join(root, name)
        if not os.path.exists(p):
            err(f"{bank}: 缺少文件 {name}"); continue
        d, qs = check_bank(bank, p, n)
        loaded[bank] = qs
        if bank in ("fill","fix") and not no_compile:
            for i,q in enumerate(qs):
                check_judge(bank, q, i, work)
        if bank in ("rutuan","zhaosheng"):
            if sum(q.get("score",0) for q in qs) != 65: err(f"{bank}: 总分 != 65")
    cross_bank_check(loaded)
    print(f"errors={len(errors)} warnings={len(warnings)}")
    for e in errors[:60]: print("  ERROR", e)
    for w in warnings[:10]: print("  WARN ", w)
    print("PASS" if not errors else "FAIL")
    return 0 if not errors else 1

if __name__ == "__main__":
    sys.exit(main())
