"""测试会话的收尾保护。

## 为什么需要这个

CI 上（`QT_QPA_PLATFORM=offscreen`）出现过这种现象：

    ............................................... [100%]
    119 passed in 0.80s
    ##[error]Process completed with exit code 139

139 = 128 + 11（SIGSEGV）。也就是说**所有测试都跑完了**，崩溃发生在
解释器退出阶段——PySide6 销毁 QApplication / Qt 平台插件时的已知问题，
与被测代码无关。

`os._exit` 会跳过 Python 与 Qt 的 teardown，从而绕开这个崩溃；
退出码仍取自 pytest 的 `exitstatus`，所以**真实失败照常是失败**。

只在 offscreen（CI 无头环境）下启用；本地开发不受影响。
"""

from __future__ import annotations

import os
import sys

import pytest


def pytest_sessionfinish(session: pytest.Session, exitstatus: int) -> None:
    if os.environ.get("QT_QPA_PLATFORM") != "offscreen":
        return
    sys.stdout.flush()
    sys.stderr.flush()
    os._exit(int(exitstatus))
