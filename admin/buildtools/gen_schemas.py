"""从 pydantic 模型生成 JSON Schema。

用途：教师手写题库 / 清单时，编辑器（VS Code 的 JSON 插件等）能补全字段、
提前发现拼写错误，而不是等导入时才报错。

一致性：`admin/tests/test_schemas.py` 会比对"重新生成的结果"与仓库里的文件，
模型改了却忘记重新生成时测试直接失败——避免 schema 与代码悄悄脱节。

用法：
    cd admin && uv run python buildtools/gen_schemas.py
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

# 直接 `python buildtools/gen_schemas.py` 时，sys.path[0] 是 buildtools/，
# 找不到 admin/app 包——把 admin/ 加进来。
_ADMIN_ROOT = Path(__file__).resolve().parents[1]
if str(_ADMIN_ROOT) not in sys.path:
    sys.path.insert(0, str(_ADMIN_ROOT))

#: 输出目录（仓库根的 schemas/）
OUT_DIR = Path(__file__).resolve().parents[2] / "schemas"

#: 生成的 schema 版本标识。改了生成逻辑就加一，便于排查"哪版生成的"。
SCHEMA_VERSION = "v1"


def _wrap(title: str, model: type) -> dict:
    schema = model.model_json_schema(mode="validation")
    schema["$schema"] = "https://json-schema.org/draft/2020-12/schema"
    schema["title"] = title
    schema["$comment"] = (
        f"由 admin/buildtools/gen_schemas.py 自动生成（{SCHEMA_VERSION}），请勿手改。"
    )
    return schema


def schemas() -> dict[str, dict]:
    """返回 `文件名 -> schema 字典`。"""
    from app.core.models import ExamBank, Manifest

    return {
        "exam-bank.schema.json": _wrap("保密考试系统 · 题库文件", ExamBank),
        "manifest.schema.json": _wrap("保密考试系统 · 构建清单", Manifest),
    }


def render_all() -> dict[str, str]:
    """返回 `文件名 -> 文件内容`（UTF-8 文本，末尾带换行）。"""
    return {
        name: json.dumps(schema, ensure_ascii=False, indent=2) + "\n"
        for name, schema in schemas().items()
    }


def main() -> int:
    # Windows 控制台默认 GBK，打印 ✓ 会 UnicodeEncodeError（build_exe.py 同款问题）
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    for name, text in render_all().items():
        path = OUT_DIR / name
        path.write_text(text, encoding="utf-8")
        print(f"✓ {path.relative_to(OUT_DIR.parent)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
