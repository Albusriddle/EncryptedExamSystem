# -*- coding: utf-8 -*-
"""代码排版修复器：把题干里"单行围栏块"的完整 C 程序美化为多行（4 空格缩进、
花括号独占行、语句逐行），并逐题编译比对输出必须完全一致，全过才写盘。

用法:
  python pretty_c.py <题库目录> --file choice.json            # 只体检（dry-run）
  python pretty_c.py <题库目录> --file choice.json --write    # 全部一致才写回
规则来源：spec §5.1；只处理含 int main 的单行块，短表达式/行内代码不动。
"""
import io, json, os, re, shutil, subprocess, sys, tempfile

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
FENCE = re.compile(r"```(?:c)?\n?(.*?)```", re.S)


def pretty(src, unit="    "):
    """把单行 C 程序排版成多行；保留字符串/字符字面量、注释与初始化列表。"""
    out, line = [], []
    indent = paren = inline_brace = 0

    def prev_char():
        for ch in reversed("".join(line).rstrip()):
            if not ch.isspace():
                return ch
        for ln in reversed(out):
            for ch in reversed(ln.rstrip()):
                if not ch.isspace():
                    return ch
        return ""

    def flush():
        nonlocal line
        text = "".join(line).strip()
        if text:
            out.append(unit * indent + text)
        line = []

    i, n = 0, len(src)
    while i < n:
        ch = src[i]
        if ch in "\"'":
            q = ch
            line.append(ch); i += 1
            while i < n:
                c = src[i]; line.append(c)
                if c == "\\" and i + 1 < n:
                    line.append(src[i + 1]); i += 2; continue
                if c == q:
                    i += 1; break
                i += 1
            continue
        if ch == "/" and i + 1 < n and src[i + 1] == "/":
            j = src.find("\n", i); j = n if j < 0 else j
            line.append(src[i:j]); i = j; continue
        if ch == "/" and i + 1 < n and src[i + 1] == "*":
            j = src.find("*/", i + 2); j = n if j < 0 else j + 2
            line.append(src[i:j]); i = j; continue
        if ch == "(":
            paren += 1; line.append(ch); i += 1; continue
        if ch == ")":
            paren = max(0, paren - 1); line.append(ch); i += 1; continue
        if ch == "{":
            if inline_brace > 0 or prev_char() in "=(,":
                inline_brace += 1; line.append(ch); i += 1; continue
            flush(); out.append(unit * indent + "{"); indent += 1; i += 1; continue
        if ch == "}":
            if inline_brace > 0:
                inline_brace -= 1; line.append(ch); i += 1; continue
            flush(); indent = max(0, indent - 1); out.append(unit * indent + "}"); i += 1; continue
        if ch == ";" and paren == 0 and inline_brace == 0:
            line.append(";"); flush(); i += 1; continue
        if ch == "\n":
            flush(); i += 1; continue
        if ch.isspace():
            if line and not line[-1].isspace():
                line.append(" ")
            i += 1; continue
        line.append(ch); i += 1
    flush()
    return "\n".join(out).strip("\n")


def run(code):
    work = tempfile.mkdtemp(prefix="qc_fmt_")
    try:
        src = os.path.join(work, "a.c"); exe = os.path.join(work, "a.exe")
        io.open(src, "w", encoding="utf-8").write(PRE + code + "\n")
        cp = subprocess.run([CC, "-O0", "-std=c11", "-w", "-o", exe, src],
                            capture_output=True, text=True, encoding="utf-8", errors="replace")
        if cp.returncode:
            return None, "compile_error:" + (cp.stderr or "")[:150]
        rp = subprocess.run([exe], input="", capture_output=True, text=True,
                            encoding="utf-8", errors="replace", timeout=5, cwd=work)
        return rp.stdout, None
    except subprocess.TimeoutExpired:
        return None, "timeout"
    finally:
        shutil.rmtree(work, ignore_errors=True)


def main():
    root = sys.argv[1] if len(sys.argv) > 1 else "."
    name = sys.argv[sys.argv.index("--file") + 1] if "--file" in sys.argv else "choice.json"
    path = os.path.join(root, name)
    d = json.load(io.open(path, encoding="utf-8"))
    changed, bad = [], []
    for q in d.get("questions") or []:
        stem = q.get("stem") or ""

        def repl(m):
            body = m.group(1)
            if len(body.strip("\n").splitlines()) != 1 or "int main" not in body:
                return m.group(0)
            old = body.strip("\n")
            new = pretty(old)
            o1, e1 = run(old)
            o2, e2 = run(new)
            if e1 or e2 or o1 != o2:
                bad.append((q["id"], e1, e2, (o1 or "")[:40], (o2 or "")[:40]))
                return m.group(0)
            changed.append((q["id"], len(old), len(new)))
            return "```c\n" + new + "\n```"

        q["stem"] = FENCE.sub(repl, stem)
    print(f"待美化={len(changed)} 输出不一致={len(bad)}")
    for x in bad[:10]:
        print("  BAD", x)
    if bad:
        print("ABORT：存在输出不一致，未写盘"); return 1
    if "--write" in sys.argv:
        io.open(path, "w", encoding="utf-8", newline="\n").write(
            json.dumps(d, ensure_ascii=False, indent=2) + "\n")
        print("已写回", path)
    else:
        print("dry-run：未写盘（加 --write 落地）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
