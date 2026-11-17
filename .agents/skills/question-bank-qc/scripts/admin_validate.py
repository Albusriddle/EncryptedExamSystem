# -*- coding: utf-8 -*-
"""管理端导入校验：走生产路径 ExamBank.model_validate + validate_bank_answers + load_paths。

用法（必须用管理端工程环境跑）：
  uv run --project D:\\DSHworkspace\\EncryptedExamSystem\\admin python scripts/admin_validate.py <题库目录>
输出：每文件一行 + load_paths 汇总 + 难度计数（独立重算，不只看 PASS）
退出码：0 = 5/5 导入且 0 答案问题；1 = 有问题
"""
import json
import sys
from collections import Counter
from pathlib import Path

from app.core.bank_import import load_paths, validate_bank_answers
from app.core.models import ExamBank

NAMES = ["choice.json", "fill.json", "fix.json", "rutuan-q16-19.json",
         "zhaosheng-b-q16-19.json"]


def main() -> int:
    root = Path(sys.argv[1] if len(sys.argv) > 1 else ".")
    files = [root / n for n in NAMES if (root / n).exists()]
    if not files:
        print(f"{root} 下没有题库文件"); return 1
    bad = 0
    for p in files:
        try:
            raw = json.loads(p.read_text(encoding="utf-8"))
            bank = ExamBank.model_validate(raw)
            problems = validate_bank_answers(bank)
            dist = dict(sorted(Counter(q.difficulty for q in bank.questions).items()))
            print(f"{p.name:<28} model_validate=OK exam_id={bank.exam_id:<22} "
                  f"pick={bank.pick:<3} q={len(bank.questions):<4} "
                  f"answer_problems={len(problems)} diff={dist}")
            for x in problems[:5]:
                print("    ANSWER", x)
            bad += len(problems)
        except Exception as exc:                      # noqa: BLE001 - 导入失败原因必须原样打印
            print(f"{p.name:<28} model_validate=FAIL {type(exc).__name__}: {exc}")
            bad += 1
    loaded, errors, warnings = load_paths(files)
    print(f"load_paths: loaded={len(loaded)} errors={len(errors)} warnings={len(warnings)}")
    for e in errors[:5]:
        print("  ERROR", e)
    for w in warnings[:5]:
        print("  WARN ", w)
    return 0 if bad == 0 and not errors else 1


if __name__ == "__main__":
    sys.exit(main())
