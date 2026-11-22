# 保密考试系统（EncryptedExamSystem）

> 面向校内闭卷 / 随堂考试的离线加密考试系统：教师在管理端编排题库与场次，生成一个自带加密数据的学生端 exe；学生在断网环境作答，答题卡加密落盘；教师收卷、校验、判分、导出成绩。
>
> 本系统的定位是**「防误操作 + 现场监考」**，不是密码学意义上的强防作弊系统。防什么、不防什么，见 [安全边界与威胁模型](docs/安全边界与威胁模型.md)；已经明确接受、且**没有**修复的风险，见 [风险接受记录](docs/风险接受记录.md)。

当前版本：**1.4.1**（`VERSION` / `Cargo.toml` / `admin/pyproject.toml` 三处一致）

| 组件 | 技术形态 | 职责 |
|---|---|---|
| `ExamAdmin` | Python + PySide6 | 教师端图形界面：题库、学生与班级、场次、打包、收卷判分、审计 |
| `exam-builder` | Rust CLI | 打包、校验包、校验答题卡、导出 PIN 表、考后查看原卷 |
| `exam-client` | Rust + egui 单文件 exe | 学生端：学号（可选 PIN）登录、网络时间门禁、离线作答、答题卡落盘 |

> 文档在 `docs/` 下（本文链接带 `docs/` 前缀），`docs/` 内各文档互为同级；另有开发用的题库质检 skill 位于 `.agents/skills/question-bank-qc/`。

---

## 1. 主要能力

### 1.1 题型与判分

支持七种题型：

| `type` | 名称 | 学生端作答方式 | 自动判分 |
|---|---|---|---|
| `single` | 单选 | 选项按钮，单选 | ✅ |
| `multiple` | 多选 | 选项按钮，多选 | ✅ |
| `judge` | 判断 | 选项按钮 | ✅ |
| `fill` | 填空（旧称补全） | 按占位符逐空输入 | ✅（可选「按项给分」） |
| `fix` | 改错 | 按行号逐条改正 | ✅（可选「按项给分」） |
| `essay` | 问答 | 多行文本框，5000 字符上限 | ❌ 人工给分 |
| `code` | 代码简答 | 等宽多行编辑器 + 行号 + 语法高亮 | ❌ 人工给分 |

- 自动判分在收卷判分页**按题型勾选**启用，默认只勾单选 / 多选 / 判断；
- 可选项：忽略大小写、忽略空白、填空 / 改错按项给分；
- 已判分也能**双击「得分」列改分**，导出里留「原分 → 新分」与「调整时间」；
- 代码题**不执行**学生代码、**不自动判分**：编译运行学生代码有安全与公平风险。

### 1.2 代码简答题

- 语言声明二选一，**不允许同时出现**：
  - `code_language`：只允许 1 种语言（字符串）；
  - `code_languages`：允许 ≥2 种语言（数组，**最多 8 个**，`uniqueItems`）；
  - 都不写 = 不限制，学生自选；
  - 两个都写 → 导入 / 打包直接报错。
- 支持 11 种：`python` / `c` / `cpp` / `java` / `rust` / `javascript` / `typescript` / `go` / `sql` / `bash` / `text`。
- 学生端语言控件**三态**：
  1. 只允许 1 种 → 灰色只读，不给选择机会；
  2. 允许 ≥2 种 → 下拉框只能从这些里选；
  3. 未限制 → 下拉框列出全部 11 种，并提示「本题老师出题时未限制语言，请以考场老师通知为准」。
- 「自动识别语言」按钮**只有学生点击时才运行**，不自动覆盖学生已选语言；识别结果若不在允许集合内，会提示「识别为 X，但本题只允许 Y」。
- 学生显式选择的语言写入答题卡 `chosen_code_languages`；导出 CSV 有「学生自报语言」列，**仅阅卷参考，不参与判分**；若超出题目允许集合，该列标注「超出允许：…」。
- 答案保留缩进、换行、行尾空格，不做 `trim`、不替换缩进、不截断；
- 容量：**单题 1 MiB**（`CODE_MAX_BYTES`）、**整卡答案 16 MiB**（`MAX_TOTAL_ANSWER_BYTES`），界面实时显示「本题 x/1024 KiB · 整卡 x/16 MiB」；
- 超过 **256 KiB** 降级为纯等宽（不做语法高亮），避免逐帧高亮卡死。

### 1.3 考试组织

- 题库导入支持两种形态：原生 JSON（题目+答案同文件）、题目 / 答案分离（`format_version: 2`，`X.questions.json` + `X.answers.json`）；
- 学生名单 / 班级（班级成员恒为全局名单子集）；
- 场次两种模式：
  - **按题库 + 平均难度区间**：`exams[]`，可设 `difficulty_min/max`，只约束抽出 N 题的**平均值**，不限制单题；
  - **按大题（section）**：`sections[]`，每个大题固定题库；大题下还可再分「部分（part）」；
- 同一学生不得同时出现在重叠场次（Rust `check_student_overlap` + Python `_check_student_overlap`）；
- 打包：班级 → 每生一个信封；可选每生 6 位 PIN；可选签名时间令牌；可选学生回看。

### 1.4 时间门禁

- 并发请求 3 个公共 HTTPS 站点的 `Date` 响应头：`www.baidu.com` / `www.aliyun.com` / `www.tencent.com`；
- 取**中位数**；成功源 ≥2 且分歧不超阈值才放行，否则 **fail-closed**；
- 本机时钟只作参考，偏差过大只警告，不参与判定；
- 可选签名时间令牌（`off` / `prefer` / `require`，默认 `off`，需自建时间服务），协议见 [时间令牌协议](docs/时间令牌协议.md)；
- 时间门禁在**派生入场密钥之前**执行（`client/src/workflow.rs::unlock` 的步骤顺序），这是必须保持的不变量。

### 1.5 收卷、判分、审计

- 答题卡 `.eea` 加密落盘，文件名 `answers-<学号>-<场次>-<试卷指纹前8位>.eea`；
- 教师端批量校验，**必须选择学生端包**（否则无法证明答题卡绑定了本包原卷）；
- 缺考名单、四类成绩 CSV + 考生成绩表；
- 审计菜单：签发答题卡收据（Ed25519）、验证收据、备份 / 恢复收据私钥、考后查看原卷；
- 哈希链审计日志 `audit-YYYYMM.jsonl`，改一行即断链。

---

## 2. 安全边界（摘要）

**已实现**

- 考试包内**不含明文标准答案**：`answer` 与 `judge` 在打包时被剥离；
- 包内只嵌**混淆后的 `K_manifest`**（密钥区 `EXAMKEY1` + 32 字节 XOR 掩码），不是 master；
- master 只在教师机；学生端不接收、不保存 master；
- 答题卡不是明文；
- 工程文件默认口令加密（`EESPROJ3`，兼容读取 `EESPROJ2`；Argon2id + AES-256-GCM）；
- 主密钥加密备份 `.eekey`；
- 时间门禁失败即拒绝；
- 代码高亮只影响显示，不执行代码。

**已接受风险（不得写成「已修复」）**

| 编号 | 内容 |
|---|---|
| R-01 | 同场学生可解出本场全部**题干**（答案 / judge 已剥离） |
| R-02 | 同场学生可读取 / 伪造他人答题卡，`check_binding` 只校验原卷指纹与 qids |
| R-03 | 默认无 PIN 时，任意已知名单内学号可解锁该生信封 |
| R-04 | Header 可被重加密，时间窗 / 回看策略只是 UI 约束，不是密码学边界 |

决策、补偿措施与复审触发条件见 [风险接受记录](docs/风险接受记录.md)。

---

## 3. 版本门禁

容器格式当前实现到 **v8**；**v9 的 P0 新格式未实现**。

| 包类型 | 容器版本 | 最低学生端 |
|---|---|---|
| 普通包 | v4 | v0.15.0+ |
| 启用签名时间令牌（`mode != off`） | v5 | v0.15.0+ |
| 按大题（`sections`） | v6 | v0.15.0+ |
| 大题下再分部分（`parts`） | v7 | v0.15.0+ |
| 含代码简答题（`qtype = code`） | v8 | v1.1.1+ |
| P0 per-student / Header 签名 | **v9（未实现）** | — |
> 表中「最低学生端」为保守口径：v0.15.0 起的客户端可读 v4–v7；更早的客户端未逐版本验证。


解析侧接受 v3–v8（`MIN_SUPPORTED_VERSION = 3`）。旧学生端遇到高于它认识的版本会**明确拒绝**，不会静默显示空白题。

答题卡：`EESANS02` / v2（v1.3.0 起新增 `chosen_code_languages`），可读取 `EESANS01` / v1 旧卡。**v1.2.x 及更早的管理端 / builder 读不了 v2 卡**，升级学生端时请同步升级管理端。

---

## 4. 快速开始（教师）

1. 从 Release 下载 `ExamAdmin-<版本>-windows.zip`，解压；
2. 进入 `ExamAdmin` 文件夹，双击 `ExamAdmin.exe`（整个文件夹要一起保留，`_internal` 不能少）；
3. **[题库]** 导入题库（可多选）；
4. **[学生]** 粘贴学号（每行一个）或按班级维护；
5. **[场次]** 新建场次：时间、名单、科目与抽题数；
6. **[打包]**：生成 / 粘贴主密钥 → 选输出 exe → 按需勾选每生 PIN、时间令牌、学生回看 → 开始打包；
7. 把生成的 `.exe` 发给学生；
8. 考后 **[收卷判分]**：拖入 `.eea` → 填 master → 选学生端包 → 校验 → 生成成绩 → 导出 CSV。

完整步骤、按钮名、常见报错对照见 [教师使用手册](docs/教师使用手册.md)。

## 5. 快速开始（学生）

- 双击学生端 exe；
- 输入学号（启用 PIN 时再输入 6 位 PIN）；
- 等待网络时间校验（至少 2 个时间源成功）；
- 作答 → 交卷（连点两次确认）；
- 答题卡自动加密保存在 exe 同目录，不可写时回退 `%LOCALAPPDATA%\ExamSystem\answers\`；
- 若教师允许，交卷后可在结束时间前只读回看。

---

## 6. 从源码构建

前置：Rust（见 `rust-toolchain.toml`）、Python 3.11+、[uv](https://github.com/astral-sh/uv)；Windows 管理端另需 PySide6 / PyInstaller。

```bash
# Rust：common / builder / client
cargo build --release --workspace

# 管理端（开发态直接跑）
cd admin
uv sync --extra gui --group dev
uv run python -m app
```

生成管理端发布目录与教师 zip：

```bash
cd admin && uv run python buildtools/build_exe.py
cd .. && bash scripts/make-release-zip.sh <版本>
```

发布前必须全绿（详见 [代码审阅报告](docs/代码审阅报告.md) 的「如何复跑」）：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd admin && uv run pytest -q
uv run ruff check . && uv run ruff format --check .
cd .. && bash scripts/verify-embed.sh
bash scripts/preflight-release.sh          # 加 --full 连管理端打包一起查
```

---

## 7. 目录结构

```text
crates/common/       共享模型、加密、容器格式、抽题、答题卡、时间门禁、审计原语
crates/builder/      exam-builder：keygen / build / verify / verify-answer-sheet /
                     review-paper / export-pins
crates/client/       学生端：GUI（egui）、解锁流程、答题卡、代码编辑器、Markdown 渲染
admin/app/core/      Qt-free 核心逻辑：models / state / grading / attendance / audit /
                     receipt / secure / bank_import / images / difficulty
admin/app/ui/        PySide6 界面：题库、学生、场次、打包、收卷判分、审计菜单
admin/buildtools/    PyInstaller 打包脚本
banks/demo/          假题库（供 CI 与演示，不含真题）
examples/            可直接导入的示例（含图片题）
docs/                本文档集（含题库规范、出题质检与难度口径）
tools/bank_qc/       题库门禁与难度评分（verify_bank / score_bank）
schemas/             题库 / manifest 的 JSON Schema（由 pydantic 生成，有漂移检查）
scripts/             verify-embed / preflight-release / make-release-zip / release_body /
                     run-admin-tests / smoke-answers
licenses/            LGPL-3.0 / GPL-3.0 全文
worker/              Cloudflare Worker 时间服务（**目录为空，未实现**）
```

---

## 8. 测试与验证基线（v1.4.1）

| 项 | 基线 |
|---|---|
| Rust | 252 个 `#[test]` |
| Python | 354 用例（`pytest` 收集口径，含参数化展开） |
| 密钥嵌入端到端 | `scripts/verify-embed.sh` 17 项 |
| 发布预检 | `scripts/preflight-release.sh` 14 项（非 `--full`） |
| CI | 7 个 job：`rust` / `python` / `release-preflight` / `admin-package` / `e2e` / `secret-scan` / `tools-scripts` |

> 数字会随提交变化，**以 CI 与脚本输出为准**。发布预检的项数看脚本最后一行
> 「✓ 发布预检通过（N 项）」；测试数量看 `cargo test` / `pytest` 的统计行。

发布包结构（`ExamAdmin-<版本>` 解压后）：

```text
ExamAdmin-<版本>/
├── 开始这里.md              ← 第一步该看什么
├── 使用说明.md              ← 完整手册（即《教师使用手册》）
├── 风险接受记录.md          ← 随包分发的安全决策
├── docs/                   ← 全部文档（手册里的链接指向这里）
├── tools/bank_qc/          ← 题库自检工具（需 Python 3.10+）
├── ExamAdmin/
│   ├── ExamAdmin.exe
│   ├── exam-builder.exe
│   ├── stub-template.exe    ← 打包用，不发给学生
│   ├── exam-server.exe      ← 网页考试服务端（可选）
│   ├── 图片使用教程.md
│   ├── _internal/
│   ├── LICENSE
│   ├── THIRD-PARTY-NOTICES.md
│   └── licenses/
└── 示例题库/
    ├── README.md           ← 说明「仅演示、门禁会 FAIL」
    ├── 数学.json
    ├── 英语.json
    └── 图片题示例.json
```

zip 旁边附带 `SHA256SUMS.txt`；构建时会自动检查包内所有 `.md` 的相对链接，有死链直接失败。

---

## 9. 文档索引

| 文档 | 读者 | 内容 |
|---|---|---|
| [教师使用手册](docs/教师使用手册.md) | 出题 / 考务老师 | 六步流程、按钮名、报错对照、安全须知 |
| [题库JSON规范](docs/题库JSON规范.md) | 出题老师 / 系统对接 | 原生与 v2 两种格式、字段、难度/语言/判题材料、导入与打包行为 |
| [出题质检与难度口径](docs/出题质检与难度口径.md) | 出题 / 质检 | K/A/C 加权、阈值、题量分布、原子化、判题回放与工具用法 |
| [图片使用教程](docs/图片使用教程.md) | 出题老师 | 在题干里插图、尺寸限额、自检清单 |
| [安全边界与威胁模型](docs/安全边界与威胁模型.md) | 维护者 / 决策者 | 资产、对手分级、承诺与不承诺 |
| [风险接受记录](docs/风险接受记录.md) | 决策者 | R-01~R-04 的事实、理由、补偿、复审条件 |
| [时间令牌协议](docs/时间令牌协议.md) | 自建时间服务的学校 | 请求 / 响应 / 验签 / 最小服务端示例 |
| [架构与实施方案](docs/保密考试系统_架构与实施方案.md) | 维护者 / 接手人 | 设计取舍、里程碑、踩过的坑 |
| [代码审阅报告](docs/代码审阅报告.md) | 维护者 | 106 轮审阅的问题索引、技术无解项、复跑方式 |
| [发布说明](docs/release-notes.md) | 教师 / 下载者 | 逐版本变更、已知限制、校验口径 |
| [题库质检 skill](.agents/skills/question-bank-qc/SKILL.md) | 出题 / 质检 | 双盲生产、门禁复核、修复复审流程 |
| [bank_qc 工具](tools/bank_qc/README.md) | 出题 / 质检 | 口径的唯一执行者；`verify_bank.py` / `score_bank.py` 用法与能力边界 |

---

## 10. 许可与致谢

本项目代码以 **Apache License 2.0** 发布（仓库根 `LICENSE`，发布包内为 `ExamAdmin/LICENSE`）。
第三方组件许可见 `THIRD-PARTY-NOTICES.md`，
随包附 `licenses/LGPL-3.0.txt`、`licenses/GPL-3.0.txt`（PySide6 / Qt 为 LGPL-3.0）。

- 创始人 / 仓库作者：[AlbusRiddle](https://github.com/Albusriddle)
- 部分创意支持：[Stone](https://github.com/stone6060)
- 精神支持：[湖南大学 Robot 工作坊](https://github.com/RobotWorkShop-BigHeroX)
