"""平均难度区间的可行性校验（Qt-free）。

难度口径（v0.7.0 起）：
- `1..255` 才是有效难度；
- `0` 与 `None` 一律视为**未标注**（旧题库里 0 曾被用作"多选"标记）。
设置区间的题库必须所有题都标注了 1..255 的难度。
"""

from __future__ import annotations

import math

from app.core.models import ExamBank


def feasible_average(diffs: list[int], k: int, lo: int, hi: int) -> bool:
    """是否存在 k 个难度的平均值落在 [lo, hi]（精确判定，位集 DP）。

    只看 min/max 会放过 {1,3} 抽 1 题、区间 2–2 这种"区间在范围内但没有
    任何子集能达到"的情况——那种配置会让学生在考场解锁时失败。
    """
    if k <= 0 or k > len(diffs):
        return False
    lo_sum = math.ceil(k * lo)
    hi_sum = math.floor(k * hi)
    if lo_sum > hi_sum:
        return False
    dp = [0] * (k + 1)
    dp[0] = 1
    for d in diffs:
        for j in range(k, 0, -1):
            dp[j] |= dp[j - 1] << d
    mask = ((1 << (hi_sum + 1)) - 1) ^ ((1 << lo_sum) - 1)
    return bool(dp[k] & mask)


def check_difficulty_range(bank: ExamBank, pick: int, lo: int, hi: int) -> list[str]:
    """返回问题列表（空 = 可以按这个区间抽题）。"""
    problems: list[str] = []
    if not (1 <= lo <= hi <= 255):
        problems.append(f"平均难度区间 {lo}–{hi} 不合法（应为 1–255 且最小值 ≤ 最大值）")
        return problems
    if pick < 1:
        problems.append("抽题数必须 ≥ 1")
        return problems
    if pick > len(bank.questions):
        problems.append(f"抽题数 {pick} 超过题量 {len(bank.questions)}")
        return problems

    missing = [q.id for q in bank.questions if not q.difficulty]
    if missing:
        sample = "、".join(missing[:5])
        more = "…" if len(missing) > 5 else ""
        problems.append(
            f"题库「{bank.title}」有 {len(missing)} 道题未标注难度（{sample}{more}），"
            "无法按平均难度抽题；请补全 difficulty（1–255）或取消该题的难度限制"
        )
        return problems

    diffs = [int(q.difficulty) for q in bank.questions if q.difficulty]
    if not feasible_average(diffs, pick, lo, hi):
        vals = sorted(diffs)
        min_mean = sum(vals[:pick]) / pick
        max_mean = sum(vals[-pick:]) / pick
        problems.append(
            f"题库「{bank.title}」抽 {pick} 题的可达平均难度为 "
            f"{min_mean:.2f}–{max_mean:.2f}，你设置的 {lo}–{hi} 无法满足"
            "（该区间内没有可行的题目组合）"
        )
    return problems
