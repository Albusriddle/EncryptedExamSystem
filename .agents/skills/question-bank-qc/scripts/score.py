# -*- coding: utf-8 -*-
"""难度评分器：K 知识点 35% + A 算法 45% + C 代码行 20%。

用法：
  python score.py <bank_dir>            # 只统计
  python score.py <bank_dir> --write    # 回写 difficulty（多选固定 9）
规则见 difficulty.md。
"""
import json, io, os, re, sys
from collections import Counter

K_CATS = {
    "类型": r"\b(int|char|float|double|long|short|unsigned|signed|void|sizeof)\b",
    "运算符": r"(\+\+|--|&&|\|\||[+\-*/%<>!=]=?|<<|>>|&|\||\^|~)",
    "控制流": r"\b(if|else|switch|case|for|while|do|break|continue|return)\b",
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

def code_of(q):
    body = q.get("body") or {}
    if body.get("kind") == "code":
        return "\n".join(body.get("code") or [])
    if body.get("kind") == "template":
        return body.get("template") or ""
    blocks = re.findall(r"```(?:c)?\n?(.*?)```", q.get("stem") or "", re.S)
    ref = (q.get("answer") or {}).get("reference") or ""
    return "\n".join(blocks + [ref])

def scoring(q):
    meta = (q.get("meta") or {}).get("scoring")
    if isinstance(meta, dict) and all(k in meta for k in ("k","a","c")) and all(isinstance(meta[k], int) for k in ("k","a","c")):
        return int(meta["k"]), int(meta["a"]), int(meta["c"]), "meta"
    text = (q.get("stem") or "") + "\n" + code_of(q)
    # K 只统计“题型相关知识点”，不把类型/运算符这类每題都出现的基础语法计入
    core = {name: pat for name, pat in K_CATS.items()
            if name in ("数组","字符串","函数","指针","结构体","宏","文件","位运算","算法")}
    cats = sum(1 for pat in core.values() if re.search(pat, text))
    k = max(1, min(3, cats))
    code = code_of(q)
    lines = [l for l in code.splitlines() if l.strip()]
    n = len(lines)
    c = 1 if n <= 3 else 2 if n <= 8 else 3 if n <= 20 else 4
    if q.get("type") == "multiple":
        return 3, 2, 1, "multi-fixed"   # 多选题真实难度统一：S=2.15
    loops = len(re.findall(r"\b(for|while|do)\b", code))
    branches = len(re.findall(r"\b(if|else|switch|case)\b", code))
    nested = bool(re.search(r"for\s*\([^)]*\)\s*\{?[^}]*for\s*\(", code, re.S)) or bool(re.search(r"while\s*\([^)]*\)\s*\{?[^}]*while\s*\(", code, re.S))
    rec = bool(re.search(r"(\w+)\s*\([^;]*\)\s*\{[^}]*\1\s*\(", code, re.S))
    if q.get("type") == "code":
        a = 4 if q.get("difficulty") == 4 else 3 if (nested or rec or loops >= 2) else 2
    elif loops == 0 and branches == 0:
        a = 1
    elif nested or rec or loops >= 2 or (loops and branches >= 2):
        a = 3
    else:
        a = 2
    return k, a, c, "heuristic"

def score(k,a,c): return 0.35*k + 0.45*a + 0.20*c

def diff_of(s, is_big=False):
    if is_big:
        return 1 if s < 1.75 else 2 if s < 2.60 else 3 if s < 3.30 else 4
    return 1 if s < 1.90 else 2 if s < 2.55 else 3

def main():
    root = sys.argv[1] if len(sys.argv) > 1 else "."
    write = "--write" in sys.argv
    for name in ("choice","fill","fix","rutuan-q16-19","zhaosheng-b-q16-19"):
        p = os.path.join(root, name + ".json")
        if not os.path.exists(p): continue
        d = json.load(io.open(p, encoding="utf-8"))
        dist = Counter(); rows = []
        # 先算 S
        calc = []
        for q in d["questions"]:
            k,a,c,src = scoring(q)
            s = score(k,a,c)
            calc.append((q,k,a,c,s,src))
        for q,k,a,c,s,src in calc:
            if q.get("type") == "multiple":
                nd = 9
            elif q.get("type") == "code":
                nd = diff_of(s, True)
            else:
                nd = diff_of(s, False)
            dist[nd] += 1
            rows.append((q["id"], k,a,c, round(s,3), nd, q.get("difficulty"), src))
            if write:
                q["difficulty"] = nd
                if q.get("type") == "code":
                    ns = {1:10,2:15,3:15,4:25}[nd]
                    q["score"] = ns
                    ans = q.get("answer") or {}
                    ans["total"] = ns
                    pts = ans.get("points") or []
                    if pts:
                        diff = ns - sum(pp.get("score",0) for pp in pts)
                        if diff:
                            pts[-1]["score"] = max(0, pts[-1].get("score",0) + diff)
                    q["answer"] = ans
                q.setdefault("meta", {})["scoring"] = {"k":k,"a":a,"c":c,"source":src}
        print(f"== {name}: n={len(d['questions'])} 分布={dict(sorted(dist.items()))}")
        print("   样例:", rows[:3])
        if write:
            io.open(p,"w",encoding="utf-8",newline="\n").write(json.dumps(d,ensure_ascii=False,indent=2)+"\n")
    print("写入" if write else "dry-run")

if __name__ == "__main__":
    main()
