# 保密考试系统（EncryptedExamSystem）

> 面向校内闭卷 / 随堂考试的离线加密考试系统：教师在管理端编排题库与场次，生成一个自带加密数据的
> 学生端程序；学生在断网环境作答，答题卡加密落盘；教师收卷、校验、判分、导出成绩。
>
> 定位是**「防误操作 + 现场监考」**，不是密码学意义上的强防作弊系统——它挡不住有合法学号的
> 学生解出本场题干（见 R-01）。防什么、不防什么见 [安全边界与威胁模型](docs/安全边界与威胁模型.md)；
> 已经明确接受、且**没有**修复的风险见 [风险接受记录](docs/风险接受记录.md)。
> **不要把它宣传成「防作弊系统」**：本系统失效的正是作弊面（同场题干可解），不是机密性。
>
> 当前版本：**1.10.0**（已发布；`VERSION` / `Cargo.toml` / `admin/pyproject.toml` 三处一致；锁文件与各文档
> 「适用版本」标记由 `scripts/bump-version.sh` 派生）。

| 组件 | 技术形态 | 职责 |
|---|---|---|
| `ExamAdmin` | Python + PySide6 | 教师端图形界面：题库、学生与班级、场次、打包、收卷判分、审计 |
| `exam-builder` | Rust CLI | 打包、校验包、校验答题卡、导出 PIN 表、考后查看原卷 |
| `exam-client` | Rust + egui 单文件程序；Windows exe / Linux ELF、AppImage | 桌面学生端：学号（可选 PIN）登录、网络时间门禁、离线作答、答题卡落盘 |
| `exam-server` | Rust 内置 HTTP/1.1 + TLS | 网页作答服务端：登录 / 取卷 / 每次保存即加密落盘 / 交卷 / 自动收卷，并带**监考页** |

> 教师包提供学生端模板（Windows `stub-template.exe`，Linux 视构建方式提供 ELF 或 AppImage 模板）。
> 学生拿到的是 `exam-builder` 生成的考试包，不是模板本身。
>
> 文档在 `docs/` 下（本文链接带 `docs/` 前缀），`docs/` 内各文档互为同级；另有开发用的题库质检
> skill 位于 `.agents/skills/question-bank-qc/`（不随发布包分发）。

---

## 1. 主要能力

### 1.1 题型与判分

支持七种题型：

| `type` | 名称 | 学生端作答方式 | 自动判分 |
|---|---|---|---|
| `single` | 单选 | 选项按钮，单选 | ✅ |
| `multiple` | 多选 | 选项按钮，多选 | ✅（集合比较，顺序无关） |
| `judge` | 判断 | 选项按钮 | ✅ |
| `fill` | 填空（旧称补全） | 按占位符逐空输入 | ✅（可选「按项给分」） |
| `fix` | 改错 | 按行号逐条改正 | ✅（可选「按项给分」） |
| `essay` | 问答 | 多行文本框，5000 字符上限 | ❌ 人工给分 |
| `code` | 代码简答 | 等宽多行编辑器 + 行号 + 语法高亮 | ❌ 人工给分 |

- 自动判分在收卷判分页**按题型勾选**启用，默认只勾单选 / 多选 / 判断；填空与改错也能自动判；
- 可选项：忽略大小写、忽略空白、填空 / 改错按项给分；
- 已判分也能**双击「得分」列改分**，导出里留「原分 → 新分」与「调整时间」；
- 代码题**不执行**学生代码、**不自动判分**：编译运行学生代码有安全与公平风险，评分依据是题库里的评分点（人工对照）。

### 1.2 代码简答题

- 语言声明二选一，**不允许同时出现**：
  - `code_language`：只允许 1 种语言（字符串）；
  - `code_languages`：允许 2–8 种语言（数组，**最多 8 个**；产品层不禁止只写 1 个，但单语言请用 `code_language`）；
  - 都不写 = 不限制，学生自选；
  - 两个都写 → 导入 / 打包直接报错： `code_language 与 code_languages 不能同时填写，二者只写一个`。
- 支持 11 种（大小写不敏感，落库前转小写）：`python` / `c` / `cpp` / `java` / `rust` / `javascript` / `typescript` / `go` / `sql` / `bash` / `text`。
- 学生端语言控件**三态**：
  1. 只允许 1 种 → 灰色只读，不给选择机会；
  2. 允许 ≥2 种 → 下拉框只能从这些里选；
  3. 未限制 → 下拉框列出全部 11 种，并提示「本题老师出题时未限制语言，请以考场老师通知为准」。
- 「自动识别语言」按钮**只有学生点击时才运行**，不自动覆盖学生已选语言；识别结果若不在允许集合内，会提示「识别为 X，但本题只允许 Y」。
- 学生显式选择的语言写入答题卡 `chosen_code_languages`；导出 CSV 有「学生自报语言」列， **仅阅卷参考，不参与判分**；若超出题目允许集合，该列标注「超出允许：…」。
- 答案保留缩进、换行、行尾空格，不做 `trim`、不替换缩进、不截断；
- 容量：**单题 1 MiB**（`CODE_MAX_BYTES`）、**整卡答案 16 MiB**（`MAX_TOTAL_ANSWER_BYTES`），界面实时显示「本题 x/1024 KiB · 整卡 x/16 MiB」；
- 超过 **256 KiB** 降级为纯等宽（不做语法高亮），避免逐帧高亮卡死，答案本身不受影响。

### 1.3 输入/输出示例（`samples`，v1.9.0 起）

题目顶层可以写结构化示例，学生端渲染成「输入 / 输出」两个等宽框：

```json
"samples": [
  {"input": "3", "output": "*\n**\n***"},
  {"input": "", "output": "EMPTY"}
]
```

| 项 | 规则 |
|---|---|
| 数量 | ≤ 20 个 |
| 大小 | 单个示例的 `input` + `output` **合计** ≤ 4096 字节（UTF-8）；中文一字 3 字节 |
| 空值 | 两者都为空的示例直接报错；`input` 为空是合法的（程序不需要输入） |
| 渲染 | 桌面端与网页端都显示「示例 N / 输入 / 输出」，单框限高 240 px，长内容在框内滚动；空串显示「（空）」 |
| 判分 | **不参与判分**，只用于展示 |
| 兼容 | 旧学生端（< v1.9.0）会忽略该字段；要与旧端混用时，建议在题干里再保留一份文字版 |
| 外部格式 | 题目 / 答案分离（`format_version: 2`）**不支持** `samples`，写了被**静默忽略**（不报错） |

> ⚠ **能过 Schema 不代表能导入**。`schemas/exam-bank.schema.json` 里 `input` / `output`
> **各自** `maxLength: 4096`，是**单字段**上限，比运行时**松**：JSON Schema 的 `maxLength` 计的是
> **字符**，运行时校验的是两字段**合计字节**。所以单字段写满 4096 个汉字的示例（≈12 KB）能过
> Schema，却会在导入 / 打包时被拒。真正生效的永远是「`input` + `output` 合计 ≤ 4096 字节」。

> 注意区分：题目顶层 `samples` 是给学生看的输入 / 输出示例，会进包；`answer.samples` 是代码题
> 评分材料里的样例，打包时随 `answer` 一起被剥离，只给教师看。

### 1.4 题干渲染（Markdown 子集）

桌面学生端只解析下面这些语法，其余按纯文本显示（不报错、不 panic）：

| 语法 | 写法 | 备注 |
|---|---|---|
| 围栏代码块 | ` ```c … ``` ` | 语言标记可省略；未闭合按普通文本 |
| 行内代码 | `` `int x` `` | 等宽 + 浅底 |
| 加粗 | `**重点**` | **不跨行**（v1.9.0 起）：`**第一行` 换行 `第二行**` 不会渲染成加粗 |
| 标题 | `# ` / `## ` / `### `（最多 6 级） | `#` 后必须有空格 |
| 列表 | `- 项` / `1. 项` | 只支持一级；续行需两个空格缩进 |
| 引用 | `> 文字` | 灰底块 |
| 表格 | 竖线表格 + 分隔行 | 缺分隔行按普通文本；`\|` 表示竖线字符 |
| 图片 | `![说明](data:image/png;base64,…)` | 只能 data URI，且独占一行 |

**不支持**：链接、斜体、删除线、嵌套列表、HTML。`![x](https://…)` 不会联网加载，只按文字显示。

> **加粗不跨行**是为输出示例服务的：一整行只有 `*` / `**` / `***` 的「星号三角形」不会再被下一行
> 吃掉（v1.9.0 修的正是这个 bug）。
>
> **网页端只渲染围栏代码块与图片**；加粗、标题、列表、表格、引用在浏览器里按纯文本（等宽）显示。
> 题干要在两种端都好看，请以「代码块 + 图片」为主，把格式信息写进文字里。

### 1.5 考试组织

- 题库导入支持两种形态：原生 JSON（题目 + 答案同文件）、题目 / 答案分离（`format_version: 2`，`X.questions.json` + `X.answers.json`）；**代码简答题只能用原生 JSON**，外部格式**不支持** `code` 题（**明确报错**）与 `samples`（**静默忽略**）；
- 学生名单 / 班级（班级成员恒为全局名单子集）；
- 场次两种模式：
  - **按题库 + 平均难度区间**：`exams[]`，可设 `difficulty_min/max`，只约束抽出 N 题的**平均值**，不限制单题；
  - **按大题（section）**：`sections[]`，每个大题固定题库；大题下还可再分「部分（part）」；
- 同一学生不得同时出现在重叠场次（Rust `check_student_overlap` + Python `_check_student_overlap`）；
- 打包：每个场次 → 名单内每生一个信封；可选每生 6 位 PIN；可选签名时间令牌；可选「仅教师可查看原卷」（默认勾选 = 学生无回看，取消勾选才允许学生回看）。

### 1.6 时间门禁

- 并发请求 3 个公共 HTTPS 站点的 `Date` 响应头：`www.baidu.com` / `www.aliyun.com` / `www.tencent.com`；
- 取**中位数**；成功源 ≥2 且分歧不超阈值才放行（3 源容差 60 秒、2 源容差 10 秒），否则 **fail-closed**；
- 本机时钟只作参考，偏差过大只警告，不参与判定；
- 可选签名时间令牌（`off` / `prefer` / `require`，默认 `off`，需自建时间服务），协议见 [时间令牌协议](docs/时间令牌协议.md)；
- 网页服务端（`exam-server`）启动时做同一套三源校验：默认 `warn` 只告警（机房 / 隔离局域网无外网是常态），`require` 不通过拒绝启动；成功时把「网络 − 本机」的偏差存成时钟偏移，之后场次判定都用校准后的时间（不改系统时钟、开考后断网也不影响）；
- 时间门禁在**派生入场密钥之前**执行（`client/src/workflow.rs::unlock` 的步骤顺序），这是必须保持的不变量。

### 1.7 网页考试与监考页

- 网页考试自 **v1.4.0** 起提供（服务端 + 管理端「网页考试…」入口）；**v1.4.0 / v1.4.1 存在题干不渲染、填空与改错判分全错的缺陷，请使用 v1.4.2 及以上**——即 **v1.4.0 提供、v1.4.2 起可用**；
- `exam-server` 在隔离局域网里提供浏览器作答入口，默认 **HTTPS（自签证书）** 8443 端口；明文端口只做 301 跳转，不承载页面与数据；
- 学生用学号（启用 PIN 时再输 PIN）登录；答案每 600 ms 防抖保存一次，**每次保存即加密落盘**；教师机崩溃 / 断电后重登可恢复未交卷答案，刷新页面自动回到原卷；
- **监考页**（v1.8.0 起）：浏览器打开启动横幅里的 `/monitor?t=…`，可看全场状态，并做 **强制收卷 / 批量收卷 / 终止 / 解除**四个写操作；
- 监考令牌独立于学生会话，存在 `<答题卡目录>/monitor.token`，不落 cookie；写操作全部记入 `events.jsonl`；
- 与网页模式相关的风险 **R-05 / R-06** 已记入 [风险接受记录](docs/风险接受记录.md)，属**已接受、未修复**，不是「已修复」。

### 1.8 收卷、判分、审计

- 答题卡 `.eea` 加密落盘，文件名 `answers-<学号>-<场次>-<试卷指纹前8位>.eea`；
- 教师端批量校验，**必须选择学生端包**（否则无法证明答题卡绑定了本包原卷）；
- 缺考名单（已交卷 / 未交卷 / 缺考 / 校验失败 / 名单外）、**五份成绩 CSV + `说明.txt`** （明细 / 汇总 / 未判分 / 全量 / 考生成绩表；`说明.txt` 为 `utf-8-sig`）；
- 审计菜单：签发答题卡收据（Ed25519）、验证收据、备份 / 恢复收据私钥、考后查看原卷；
- 哈希链审计日志 `audit-YYYYMM.jsonl`（blake2s-128），改一行即断链。

---

## 2. 安全边界（摘要）

**已实现**

- 考试包内**不含明文标准答案**：`answer` 与 `judge` 在打包时被剥离；
- 包内只嵌**混淆后的 `K_manifest`**（密钥区 `EXAMKEY1` + 32 字节 XOR 掩码），不是 master；
- master 只在教师机；学生端不接收、不保存 master；
- 答题卡不是明文；
- 工程文件默认口令加密（`EESPROJ3`，兼容读取 `EESPROJ2`；Argon2id + AES-256-GCM）；主密钥可导出 `.eekey` 加密备份；
- 时间门禁失败即拒绝；
- 网页状态文件权限收紧：Windows 受保护 DACL（仅当前用户）、Unix `0600`；
- 网页登录限速按**真实对端 IP** 分桶（默认 60 次/分钟，可配置）；监听地址可只绑局域网网卡；
- 代码高亮只影响显示，不执行代码。

**已接受、未修复的风险（不得写成「已修复」）**

| 编号 | 内容 |
|---|---|
| R-01 | 同场学生可解出本场全部**题干**（答案 / judge 已剥离） |
| R-02 | 同场学生可读取 / 伪造他人答题卡，`check_binding` 只校验原卷指纹与 qids |
| R-03 | 默认无 PIN 时，任意已知名单内学号可解锁该生信封 |
| R-04 | Header 可被重加密，时间窗 / 回看策略只是 UI 约束，不是密码学边界 |
| R-05 | 网页考试模式：自签 TLS 挡被动嗅探但不挡主动中间人（未核对指纹时）、浏览器不可控、教师机单点 |
| R-06 | 监考页在明文模式（`--tls off`）下**默认允许写操作**（`--monitor-write-mode tls-only` 可关），令牌可被同网段嗅探 |

**明确不承诺**：不防伪造、不防同场协作、不证明答题卡作者身份、不阻止 DevTools / 切屏 / 截图 / 搜索，也不对抗任何形式的作弊；答题卡收据只能证明「收卷之后没被改」。

决策、补偿措施与复审触发条件见 [风险接受记录](docs/风险接受记录.md)；威胁模型见 [安全边界与威胁模型](docs/安全边界与威胁模型.md)。

---

## 3. 版本门禁

容器格式当前实现到 **v8**；**v9 的 P0 新格式未实现**。

| 包类型 | 容器版本 | 最低学生端 |
|---|---|---|
| 普通包 | v4 | 未逐版本验证（保守写 v0.15.0+） |
| 启用签名时间令牌（`mode != off`） | v5 | 未逐版本验证（保守写 v0.15.0+） |
| 按大题（`sections`） | v6 | **v0.14.0+** |
| 大题下再分部分（`parts`） | v7 | **v0.15.0+** |
| 含代码简答题（`qtype = code`） | v8 | **v1.1.1+** |
| P0 per-student / Header 签名 | **v9（未实现）** | — |

> v6 在 **v0.14.0** 就能读（按大题是那一版引入的），v7 才需要 v0.15.0；v4 / v5 的下界没有逐版本
> 验证过，因此保守写成 v0.15.0+。解析侧接受 v3–v8（`MIN_SUPPORTED_VERSION = 3`）。旧学生端遇到
> 高于它认识的版本会**明确拒绝**（`不支持的版本 N（当前支持 3–8）`），不会静默显示空白题。

答题卡：`EESANS02` / v2（v1.3.0 起新增 `chosen_code_languages`），可读取 `EESANS01` / v1 旧卡。 **v1.2.x 及更早的管理端 / builder 读不了 v2 卡**，升级学生端时请同步升级管理端。

语言下拉框与「自动识别语言」按钮需 **v1.3.0+** 学生端；`samples` 示例块需 **v1.9.0+** 学生端；学生端题号旁的 `#题库ID` 显示为 **v1.10.0** 新增（桌面端与网页端都有）。

> **本版（v1.10.0）的三个关键变化**：Linux 原生运行 / AppImage / 默认示例题库（Linux 产物不随 GitHub Release 分发）；学生端题号旁显示 `#题库ID`（桌面端与网页端）；`exam-builder` 移除命令行 `--master`（master 只走 `EXAM_MASTER` 或 `--master-file`）。

---

## 4. 快速开始（教师）

1. Windows 从 GitHub Release 下载 `ExamAdmin-<版本>-windows.zip` 并解压； Linux 从源码构建，或从 GitHub Actions 的 `linux-appimage` job 下载 `exam-admin-linux-appimage` 制品（内含 AppImage；见 [Linux 桌面指南](docs/arch-linux.md)）——**Linux 产物不随 Release 分发**；
2. Windows 双击 `ExamAdmin/ExamAdmin.exe`（保留整个文件夹）；Linux 在文件属性中允许执行，然后双击 AppImage，可从「帮助 → 创建桌面启动器」添加应用菜单入口；
3. **[题库]** 默认已加载数学、英语与题型展示示例（3 库 14 题）；可直接试用，正式考试请先移除示例再导入自己的题库（可多选）；
4. **[学生]** 粘贴学号（每行一个）或按班级维护；
5. **[场次]** 新建场次：时间、名单、科目与抽题数；
6. **[打包]**：生成 / 粘贴主密钥 → 选输出学生包 → 按需勾选每生 PIN、时间令牌、「仅教师可查看原卷」（默认勾选 = 学生无回看）→ 开始打包；
7. Windows 发生成的 `.exe`；Linux AppImage 桌面版默认生成 `StudentClient.AppImage`，只需分发这一份文件；
8. 考后 **[收卷判分]**：拖入 `.eea` → 填 master → 选学生端包 → 校验 → 生成成绩 → 导出 CSV。

完整步骤、按钮名、常见报错对照见 [教师使用手册](docs/教师使用手册.md)。

## 5. 快速开始（学生）

- 双击学生端 `.exe`（Windows）或 `.AppImage`（Linux；先允许执行），输入学号（启用 PIN 时再输入 6 位 PIN）；题号旁会显示 `#题库ID`——它是**题目**的 `id`（如 `#q16`），不是导入时填的**题库 ID**（`exam_id`，用作文件名）；
- 等待网络时间校验（至少 2 个时间源成功）；作答 → 交卷（**再点一次确认，5 秒内有效**；另有「取消」按钮）；
- 答题卡自动加密保存在学生包同目录；不可写时 Windows 回退 `%LOCALAPPDATA%\ExamSystem\answers\`， Linux 回退 `${XDG_DATA_HOME:-~/.local/share}/ExamSystem/answers/`；
- 若教师允许，交卷后可在结束时间前只读回看。

---

## 6. 从源码构建

### Arch Linux（原生运行，无需 Wine）

安装 Arch 依赖后，在仓库根执行：

```bash
bash scripts/build-linux.sh              # Rust 产物 + 隔离的 Python GUI 环境 + 自检
bash scripts/run-admin.sh                # 教师管理端
# 推荐桌面分发：生成教师 AppImage、学生 AppImage 模板和 Linux ZIP
bash scripts/build-linux.sh --appimage
# 或仅生成传统目录式 Linux ZIP
bash scripts/build-linux.sh --package
```

依赖安装、Wayland/X11、学生端运行、XDG 数据目录与排错见 **[Arch Linux 指南](docs/arch-linux.md)**。AppImage 版包含中文字体与学生模板，教师和学生都可双击启动，无需安装 Python 或 Wine；传统无扩展名 ELF 仍可用。请为不同操作系统分别构建和分发。 Arch 本机构建不等于兼容旧发行版；CI 的 `linux-appimage` job 使用 Ubuntu 22.04 基线，上传 `exam-admin-linux-appimage` 供下载；`release.yml` 不发布 Linux 产物。跨发行版支持仍需在目标系统验收。

### 通用构建

前置：Rust（见 `rust-toolchain.toml`）、Python 3.11+、[uv](https://github.com/astral-sh/uv)； Windows 管理端另需 PySide6 / PyInstaller。

```bash
# Rust：common / builder / client / server
cargo build --release --workspace

# 管理端（开发态直接跑）
cd admin
uv sync --extra gui --group dev
uv run python -m app.main
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
bash scripts/run-admin-tests.sh            # Windows 上按 JUnit 结论，避免 Qt teardown 127/139 误判
cd admin && uv run ruff check . && uv run ruff format --check .
cd .. && bash scripts/verify-embed.sh
bash scripts/preflight-release.sh          # 加 --full 连管理端打包一起查
```

---

## 7. 目录结构

```text
crates/common/       共享模型、加密、容器格式、抽题、答题卡、时间门禁、审计原语
crates/builder/      exam-builder：keygen / build / verify / verify-answer-sheet /
                     review-paper / export-pins
crates/client/       桌面学生端：GUI（egui）、解锁流程、答题卡、代码编辑器、Markdown 渲染
crates/server/       exam-server：内置 HTTP/1.1 + TLS 的网页作答服务端与监考页
                     src/：main / tls / clock / secure_fs / monitor；assets/：app.js、monitor.js、codemirror 等
admin/app/core/      Qt-free 核心逻辑：models / state / grading / audit / receipt / secure /
                     bank_import / images / difficulty / default_banks（新工程示例与快照）
admin/app/ui/        PySide6 界面：题库、学生、场次、打包、收卷判分、审计菜单
admin/buildtools/    PyInstaller / Linux AppImage 打包脚本
banks/demo/、examples/  假题库（CI/演示，不含真题）与可直接导入的示例（含图片题）
docs/                本文档集（含题库规范、出题质检与难度口径）
tools/bank_qc/       题库门禁与难度评分（verify_bank / score_bank）
schemas/             题库 / manifest 的 JSON Schema（由 pydantic 生成，有漂移检查）
scripts/             build-linux / run-admin / verify-embed / preflight-release / make-release-zip /
                     run-admin-tests / smoke-answers / smoke-web / smoke-appimage / smoke-monitor /
                     smoke-tls-cert / drill-web / bump-version
licenses/            LGPL-3.0 / GPL-3.0 / ISC 全文
worker/              Cloudflare Worker 时间服务（只有空的 `worker/src`，未实现）
```

> `exam-server` 的完整参数表、TLS 实现细节与已知限制写在仓库内的 `crates/server/README.md`
> （**只写路径、不做成链接**：发布包除包根入口（`开始这里.md` / `使用说明.md` / `风险接受记录.md`）
> 与 `ExamAdmin/` 外，只带 `docs/`、`tools/`、`示例题库/`；指向 `crates/` 的链接在包里就是死链，
> `make-release-zip.sh` 会直接判失败）。

---

## 8. 测试与验证基线（v1.10.0）

| 项 | 基线 |
|---|---|
| Rust | Arch Linux：402 passed；Windows 由 `admin-package` 的 `cargo test` 覆盖，各平台以输出为准 |
| Python | Arch Linux：538 passed、0 skipped；Windows：529 passed + 9 skipped（PySide6 offscreen） |
| 密钥嵌入端到端 | `scripts/verify-embed.sh` 17 项 |
| 发布预检 | `scripts/preflight-release.sh` 18 项（非 `--full`） |
| CI | 9 个 job：`rust` / `python` / `linux-appimage` / `release-preflight` / `admin-package` / `e2e` / `secret-scan` / `tools-scripts` / `web-smoke` |

> 数字会随提交变化，**以 CI 与脚本输出为准**。发布预检的项数看脚本最后一行
> 「✓ 发布预检通过（N 项）」；测试数量看 `cargo test` / `pytest` 的统计行。

---

## 9. 发布包结构

Windows ZIP 解压后的 `ExamAdmin-<版本>/`（Linux ZIP 的入口结构相同，程序改用 Linux 文件名）：

```text
ExamAdmin-<版本>/
├── 开始这里.md              ← 第一步该看什么
├── 使用说明.md              ← 完整手册（即《教师使用手册》）
├── 风险接受记录.md          ← 随包分发的安全决策
├── docs/                   ← 使用说明里的链接指向这里
├── tools/bank_qc/          ← 题库自检工具（需 Python 3.10+，只用标准库）
├── ExamAdmin/
│   ├── ExamAdmin.exe
│   ├── exam-builder.exe
│   ├── stub-template.exe    ← 打包用的学生端模板，不发给学生
│   ├── 图片使用教程.md
│   ├── LICENSE / THIRD-PARTY-NOTICES.md / licenses/
│   └── _internal/
│       └── exam-server.exe  ← 网页考试服务端（PyInstaller 打进 _internal）
└── 示例题库/
    ├── README.md           ← 说明「仅演示、门禁会 FAIL」
    ├── 数学.json
    ├── 英语.json
    └── 图片题示例.json
```

- `docs/` 随 ZIP 分发，共 **11 份**（9 份产品文档 + `release-notes.md` + `代码审阅报告.md`），收录教师手册、题库规范、安全文档、发布说明及 [Arch Linux 指南](docs/arch-linux.md) 等文档；**仓库 `docs/` 下的每一份 `.md` 都会进包**，所以链接必须能在包里打开；
- **发布包根目录不含本 `README.md`**（它只面向仓库读者）；包内可见的入口是 `开始这里.md` 与 `使用说明.md`；
- `示例题库/` 是可**手动导入**的文件（包括图片题示例），不是新工程自动加载的内置示例。两者都仅供演练，**不满足出题口径**：对 `示例题库/` 跑 `verify_bank.py` 会得到 `FAIL`，这是预期结果；
- 内置默认示例是 `banks/demo/` 的 **3 库 14 题**；包内 `示例题库/` 是另一套 **3 库 11 题**（`exam_id` 不同），**不要同时导入**；
- zip 旁边附带 `SHA256SUMS.txt`；`make-release-zip.sh` 构建时会自动检查包内**所有 `.md`** 的相对链接，有死链直接失败（代码围栏数量不成对也会失败）。

---

## 10. 已知边界（能力边界，不是待办遗漏）

| 项 | 现状与原因 |
|---|---|
| 真机 30–50 台演练 | 脚本能力在（`drill-web.sh` 无人数上限，CI 只跑 12 人）；**真机规模需要现场组织**，历来由使用方在开考前自行演练 |
| 浏览器本地草稿（IndexedDB） | 未做：服务端是唯一权威副本，现在只靠 `keepalive` 补「关页前最后 0–600 ms」；断网期间未同步的那几题会丢 |
| 静态资源 keep-alive | 未做：每个资源单独建连（一次页面加载约 6–7 次 TLS 握手）。局域网 TLS 1.3 只需 1 RTT，收益与「改连接模型」的风险不匹配；第三方 vendored 资源（`codemirror` / `clike` / `python`）已允许缓存 1 小时 |
| 外部证书精确到期日 | 未做：`rustls` 不暴露 `notAfter`，自检只报「是否过期」；要打印日期得引 X.509 解析依赖，不划算 |
| 逐题难度上下限过滤 | 刻意不做：难度区间只约束**平均值**，给教师留「难易搭配」空间 |
| P0 的 per-student / Header 签名 | **v9 未实现**；当前容器最高 v8 |

---

## 11. 文档索引

| 文档 | 读者 | 内容 |
|---|---|---|
| [教师使用手册](docs/教师使用手册.md) | 出题 / 考务老师 | 六步流程、按钮名、网页考试入口、报错对照、安全须知 |
| [题库JSON规范](docs/题库JSON规范.md) | 出题老师 / 系统对接 | 原生与 v2 两种格式、字段、`samples`、语言、判题材料、导入与打包行为 |
| [出题质检与难度口径](docs/出题质检与难度口径.md) | 出题 / 质检 | K/A/C 加权、阈值、题量分布、原子化、判题回放与工具用法 |
| [图片使用教程](docs/图片使用教程.md) | 出题老师 | 在题干里插图、尺寸限额、自检清单 |
| [安全边界与威胁模型](docs/安全边界与威胁模型.md) | 维护者 / 决策者 | 资产、对手分级、承诺与不承诺 |
| [风险接受记录](docs/风险接受记录.md) | 决策者 | R-01~R-06 的事实、理由、补偿、复审条件 |
| [时间令牌协议](docs/时间令牌协议.md) | 自建时间服务的学校 | 请求 / 响应 / 验签 / 最小服务端示例 |
| [架构与实施方案](docs/保密考试系统_架构与实施方案.md) | 维护者 / 接手人 | 设计取舍、里程碑、踩过的坑 |
| [代码审阅报告](docs/代码审阅报告.md) | 维护者 | 审阅轮次的问题索引、技术无解项、复跑方式（轮次以该报告为准） |
| [发布说明](docs/release-notes.md) | 教师 / 下载者 | 逐版本变更、已知限制、校验口径 |
| [Arch Linux 指南](docs/arch-linux.md) | Linux 教师 / 学生 / 维护者 | 原生构建、依赖、AppImage、Wayland/X11、XDG 路径与排错 |
| [题库质检 skill](.agents/skills/question-bank-qc/SKILL.md) | 出题 / 质检 | 双盲生产、门禁复核、修复复审流程 |
| [bank_qc 工具](tools/bank_qc/README.md) | 出题 / 质检 | 口径的唯一执行者；`verify_bank.py` / `score_bank.py` 用法与能力边界 |

---

## 12. 许可与致谢

本项目代码以 **Apache License 2.0** 发布（仓库根 `LICENSE`，发布包内为 `ExamAdmin/LICENSE`）。第三方组件许可见 `THIRD-PARTY-NOTICES.md`，随包附 `licenses/LGPL-3.0.txt`、 `licenses/GPL-3.0.txt`（PySide6 / Qt 为 LGPL-3.0）与 `licenses/ISC.txt`（`exam-server` 静态链接的 `ring` 为 Apache-2.0 AND ISC）。

- 创始人 / 仓库作者：[AlbusRiddle](https://github.com/Albusriddle)
- 部分创意支持：[Stone](https://github.com/stone6060)
- 精神支持：[湖南大学 Robot 工作坊](https://github.com/RobotWorkShop-BigHeroX)
