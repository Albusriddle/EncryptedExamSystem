# AGENTS.md — AI / 自动化代理在本仓库的工作规则

> **本文件是强制性的。** 任何在本仓库工作的 AI 代理（以及人类贡献者）都必须遵守。
> 违反标注 **MUST / MUST NOT** 的条目，其改动一律视为不可合并。
>
> 若本文件与用户当轮的明确指令冲突，**以用户指令为准**，但必须在回复中说明冲突点与影响。

---

## 0. 开工前必读

按顺序读完再动手，不要凭印象改代码：

1. `README.md` —— 项目定位与能力边界
2. `docs/保密考试系统_架构与实施方案.md` —— 设计取舍
3. 本文件
4. 你要改的那个模块的现有注释 —— **本仓库的注释记录了大量踩坑历史，改之前先读懂"为什么这样写"**

---

## 1. 工作流：先方案，后执行

- 用户说「**审阅 / review / 看看有什么问题**」时：**只审阅**，输出问题清单与方案，**MUST NOT** 直接改代码。
- 修复前 MUST 给出方案（含取舍与替代方案），得到明确「执行」后再动手。
- 用户说「**在推进工程进度前停下**」时：完成当前修复即停，**MUST NOT** 顺手加新功能。
- 一次提交只做一件事。**MUST NOT** 把重构、格式化、功能、修复混在一个提交里。
- **MUST NOT** force-push `main`；**MUST NOT** 改写已发布 tag 的历史。

---

## 2. 提交前必须全绿（MUST）

任何一项失败都**不得**提交、不得打 tag：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                     # 必须全部通过
cd admin && uv run pytest -q               # 必须全部通过
uv run ruff check . && uv run ruff format --check .
bash scripts/verify-embed.sh               # 全过
bash scripts/preflight-release.sh          # 全过
```

提交信息里 MUST 写明实际跑过的验证与结果。**MUST NOT** 声称"应该没问题"。

---

## 3. 测试规则（MUST）

- 修 bug MUST 附带**能复现该 bug 的回归测试**。正确顺序：先写出失败的测试 → 确认它确实失败 → 再修 → 确认转绿。
- 测试 MUST 断言**用户可见行为**，不能只断言内部数据结构。
  > 反例（真实教训）：场次保存后界面勾选消失，而只断言 `state.sessions[0].exams` 的用例一直是绿的。
- 涉及 Qt 的测试 MUST 以 `pytest.importorskip("PySide6")` 开头：CI 已安装 PySide6
  并以 `QT_QPA_PLATFORM=offscreen` 真实运行，但缺 GUI 依赖的环境（如精简容器）
  应能跳过而不是整批失败。
- 涉及信号联动的 UI bug，测试 MUST 复刻真实接线（如 `tab.changed.connect(refresh_choices)`），否则复现不出来。
- **MUST NOT** 为了让测试通过而放宽断言、删除用例、跳过测试。
- 测试写磁盘时 **MUST NOT** 用 `std::env::temp_dir()`（本机 MSYS 下指向 `/tmp`，原生进程写不进去）；用 `target/` 下的目录。

---

## 4. 验证规则（MUST）

- **MUST NOT 用屏幕截图验证 GUI**。DPI 缩放与多显示器会给出完全错误的结论（本项目已因此误判 3 次，把"布局正常"看成"被裁切"）。
  必须用程序自报：
  | 对象 | 手段 |
  |---|---|
  | 管理端界面 | `ExamAdmin.exe --dump-ui <dir>`（自绘 PNG + `info.json`） |
  | 学生端弹窗/布局 | **debug 构建** + `EES_DEBUG_LAYOUT=<file>` |
  | 学生端题号卡 | **debug 构建** + `EES_NAV_JUMP=<n>` + `EES_DEBUG_NAV=<file>` |
  | 管理端弹窗是否存在 | 枚举进程可见顶层窗口数（主窗口 + 对话框 = 2）；**注意带 owner 的对话框也算** |

- 验收对象 MUST 是**发布包**（从 GitHub Release 下载的那份），不能只测本地源码构建。
- 学生端 GUI 验证前 MUST 先删 `%LOCALAPPDATA%\ExamSystem\skip_welcome`，否则欢迎弹窗不出现。
- 布局类结论 MUST 用具体数值（rect 坐标、`contains`、`center_offset`）支撑，不能写"看起来正常"。

---

## 5. 安全红线（MUST NOT）

- **MUST NOT** 把 master 密钥、`kf.txt`、真实题库、学生名册提交入库。
  `.gitignore` 已覆盖 `kf.txt` / `m[0-9].json` / `*.key` / `bank*.json` / `examadm-*/`。
- master **MUST** 只通过环境变量 `EXAM_MASTER` 传给子进程；**MUST NOT** 出现在命令行参数（进程列表可见）或子进程输出里。日志输出 MUST 经过打码。
  - **已批准的例外**：管理端「打包」页会把 master 显示在日志区，供教师复制保存
    （经用户明确要求）。该显示只发生在 GUI 文本区（不落盘、不进子进程输出），
    MUST 紧跟醒目警告说明泄漏后果。除此之外任何地方出现 master 都算违规。
- **MUST NOT** 让标准答案进入 `Paper` / `PaperItem`。类型层面已保证（这两个结构没有 `answer` 字段），不要为了方便加回去。
- 写临时题库文件时 **MUST** 剥掉 `answer`，并在打包结束（成功/失败/取消）后删除临时目录。
- **MUST NOT** 引入"可由包内配置指定时间源"的能力（`time_sources` 曾因此被删除）：那等于给时间门禁开后门。
- **MUST NOT** 在 release 构建里保留可用的调试开关。所有 `EES_*` 环境变量 MUST 经 `diag_env` / `diag_path`（`#[cfg(debug_assertions)]`）访问。
- 包内解析出的参数都是**不可信输入**（KDF 参数、salt、偏移、长度），MUST 先校验范围/边界再用，**MUST NOT** 出现可 panic 的路径。

---

## 6. 必须保持的不变量（MUST）

| 不变量 | 位置 |
|---|---|
| 时间门禁在派生入场密钥**之前** | `client/src/workflow.rs::unlock` 的步骤顺序 |
| 同 `(student_id, session_id, salt)` 必得同一份试卷 | `common/src/picker.rs`，改动需保持 `GOLDEN_FP` 或显式说明 |
| 试卷指纹只哈希题目 **ID** | 修改会导致已发试卷失效 |
| `VERSION` 是唯一版本来源，三处同步 | `VERSION` / `Cargo.toml` / `admin/pyproject.toml` |
| 对外文案唯一来源 | `branding.json`（Rust `include_str!`，Python 运行时读） |
| Python 与 Rust 的校验规则一致 | `ProjectState.validate()` ↔ `validate_sessions_impl()` |
| `admin/app/core/` 不依赖 Qt | 核心逻辑可脱离 GUI 独立测试 |
| 时间必须带显式时区 | 两端都拒绝无偏移时间（会被当成 UTC，静默偏移 8 小时） |
| 同一学生不得同时出现在重叠场次 | Rust `check_student_overlap` + Python `_check_student_overlap` |

---

## 7. 代码风格

**Rust**
- `rustfmt` 默认格式；`clippy -D warnings` 必须干净。
- 错误用 `anyhow`（应用层）/ `thiserror`（库层）。**MUST NOT** 对用户输入 `unwrap()`。
- 注释写**为什么**，不写"是什么"。踩过的坑要写进注释（本仓库已有大量先例）。

**Python**
- `ruff`，行宽 100，`select = ["E","F","I","UP","B"]`。
- `app/core/` MUST 保持 Qt-free；GUI 相关只能放 `app/ui/`。
- 用户可见的报错 MUST 说明"发生了什么 + 怎么办"，不能只抛原始异常。

**通用**
- 中文注释与中文提交信息；技术术语保留英文原文。
- 注释里不要写会过期的数字（如版本号、测试数量）；必需时用占位或从源头读取。

---

## 8. 提交与发布

- 提交身份：`Albusriddle <albusriddle@foxmail.com>`。
- 提交信息：中文，首行 `type(scope): 摘要`（type ∈ feat/fix/docs/test/chore/refactor/perf），正文写**根因**、**方案**、**验证结果**。
- 发布流程：
  1. 改 `VERSION` + `Cargo.toml` + `admin/pyproject.toml`
  2. 全量验证全绿
  3. `git tag v<版本>` 并推送 tag
  4. 等 `release` 工作流完成，**核对资产齐全**（zip + 两个 exe）
- 只改文档时 **不需要** 发新版本，直接提交推送即可。

---

## 9. 禁止事项速查

| 禁止 | 原因 |
|---|---|
| 提交密钥/真题/名册 | 公开仓库，永久泄漏 |
| 用截图判断 GUI 布局 | DPI 下结论不可靠，已误判多次 |
| 只断言模型、不断言界面 | 会出现"数据对、界面错"的绿测试 |
| 在 `admin/app/core/` import Qt | 核心逻辑要保持可独立测试（CI 现在装了 Qt，但分层不能破） |
| 在 release 里留 `EES_*` 开关 | 把测试钩子发给学生 |
| 允许包内自定义时间源 | 时间门禁后门 |
| 用字符串比较时间 | 跨时区会判错 |
| 把 `exam_id` 直接拼进文件路径 | 路径穿越（已发生过） |
| 无上限使用包内的 KDF 参数 | 数 TB 内存分配 → abort |
| 未经确认改写已发布 tag | 破坏他人已下载的包 |

---

## 10. 交付回复的格式要求

完成一轮工作后，回复 MUST 包含：

1. **做了什么**（对应问题编号）
2. **根因**（不是"改了哪里"，而是"为什么会坏"）
3. **验证证据**（实际命令 + 实际输出，或矩形数值）
4. **仍未解决的**（如实列出，不要粉饰）

**MUST NOT** 使用"应该可以""理论上没问题"这类无证据表述。
