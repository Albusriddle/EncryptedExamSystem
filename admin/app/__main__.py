"""让文档中的 ``python -m app`` 与冻结后的入口使用同一套启动逻辑。"""

from app.main import main

raise SystemExit(main())
