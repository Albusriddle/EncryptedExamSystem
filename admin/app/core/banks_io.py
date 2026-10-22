"""题库文件读写的纯逻辑（**不依赖 Qt**）。

独立于 `ui/` 的原因：这些函数关系到"标准答案会不会落盘"，
必须有回归测试守着；而 CI 的 python job 不安装 PySide6，
若写在 `ui/tab_build.py` 里，测试一导入就会 ModuleNotFoundError。
"""

from __future__ import annotations

from pathlib import Path

from app.core.models import ExamBank


def write_temp_banks(banks: dict[str, ExamBank], tmpdir: Path) -> None:
    """把题库写到临时目录，**剥掉 answer**。

    打包时 builder 本来也会剥离答案，这里提前剥是纵深防御：
    万一 builder 被绕过或将来改动，临时文件本身也是干净的。

    旧实现直接 `bank.model_dump_json()` 落盘，把标准答案明文写进
    `%TEMP%\\examadm-*\\*.json`，而且从不删除——等于每次打包都把答案
    永久留在教师机器上。
    """
    for exam_id, bank in banks.items():
        stripped = bank.model_copy(deep=True)
        for q in stripped.questions:
            q.answer = None
        (tmpdir / f"{exam_id}.json").write_text(
            stripped.model_dump_json(by_alias=True), encoding="utf-8"
        )
