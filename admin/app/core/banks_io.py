"""题库文件读写的纯逻辑（**不依赖 Qt**）。

独立于 `ui/` 的原因：这些函数关系到"标准答案会不会落盘"，
必须有回归测试守着；写成 Qt-free 的纯函数后，测试不依赖 GUI，
本地与 CI 都能直接跑（CI 现在也装了 PySide6 并真实运行 UI 测试，
但核心逻辑保持 Qt-free 仍然是好设计：依赖越少越稳）。
"""

from __future__ import annotations

from pathlib import Path

from app.core.models import ExamBank, validate_exam_id


def write_temp_banks(banks: dict[str, ExamBank], tmpdir: Path) -> None:
    """把题库写到临时目录，**剥掉 answer 与 judge**。

    打包时 builder 本来也会剥离答案，这里提前剥是纵深防御：
    万一 builder 被绕过或将来改动，临时文件本身也是干净的。

    旧实现直接 `bank.model_dump_json()` 落盘，把标准答案明文写进
    `%TEMP%\\examadm-*\\*.json`，而且从不删除——等于每次打包都把答案
    永久留在教师机器上。
    """
    for exam_id, bank in banks.items():
        # 纵深防御：模型层已经用白名单限制过 exam_id，这里再挡一次。
        # 只要最终文件名不等于 `exam_id.json`，就说明存在路径成分。
        name = f"{validate_exam_id(exam_id)}.json"
        # 双保险：最终文件名必须等于"裸文件名"，否则说明仍含路径成分
        if Path(name).name != name:
            raise ValueError(f"题库 exam_id 含非法路径成分，拒绝写入: {exam_id!r}")
        stripped = bank.model_copy(deep=True)
        for q in stripped.questions:
            q.answer = None
            # judge 里有完整 main() 与预期输出，同样绝不能进包
            q.judge = None
        # exclude_none：答案/判分材料是 None 时连键都不写，
        # 而不是留一个 `"judge": null`——少一个字段就少一次被误读的机会。
        (tmpdir / name).write_text(
            stripped.model_dump_json(by_alias=True, exclude_none=True), encoding="utf-8"
        )
