# EncryptedExamSystem · 保密考试系统

[![CI](https://github.com/Albusriddle/EncryptedExamSystem/actions/workflows/ci.yml/badge.svg)](https://github.com/Albusriddle/EncryptedExamSystem/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/Albusriddle/EncryptedExamSystem)](https://github.com/Albusriddle/EncryptedExamSystem/releases/latest)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

> 管理端编排考试 → 打包加密题库 → 生成**单个自解压 exe** →
> 学生凭 **学号 + 网络时间门禁** 解锁 → 确定性抽题，生成个人试卷。

**无需服务器、无需部署数据库。** 一场考试就是一个 exe 文件。

| 你是 | 从这里开始 |
|---|---|
| 老师 | [教师使用手册](docs/教师使用手册.md)（不需要懂编程） |
| 想了解设计 | [架构与实施方案](docs/保密考试系统_架构与实施方案.md) |
| 想改代码 | [AGENTS.md](AGENTS.md)（**强制规则**，先读再动手） |

---

## 下载

最新版本见 [Releases](https://github.com/Albusriddle/EncryptedExamSystem/releases/latest)。

| 资产 | 给谁 | 说明 |
|---|---|---|
| `ExamAdmin-<版本>-windows.zip` | 教师 | 解压后进 `ExamAdmin/` 双击 `ExamAdmin.exe` |
| `exam-builder.exe` | 进阶用户 / 脚本 | 命令行打包器 |
| `stub-template.exe` | 进阶用户 | 学生端模板，打包时被复制并追加加密数据 |

发布包内已含示例题库，解压即可跑通全流程。

---

## 教师工作流

1. **题库** —— 导入 JSON 题库（格式见手册），支持多题库、按题库设默认抽题数
2. **学生** —— 粘贴或导入 CSV 学号名单
3. **场次** —— 编排考试时间窗、勾选科目、设每场抽题数；时间轴可视化重叠场次
4. **打包** —— 点「生成」得到学生端 exe，分发即可
5. 学生双击 → 输学号 → 通过时间门禁 → 看到自己的试卷
6. **收卷判分** —— 拖入 `.eea` 校验，自动判分（可改分），导出已判/未判 CSV

学生端**只需要联网校时**（三个公共时间源取中位数），不访问任何自建服务。

---

## 当前能力边界

**已实现**：多场次 · 按场次选科与抽题 · 学号名单（可建班级分组） · 题干 Markdown（代码块/表格/图片，管理端内置「插入图片…」，教程见 `docs/图片使用教程.md`） · 网络时间门禁 · 确定性出卷 · 试卷指纹 ·
学生端右侧题号导航卡（圆形按钮，点题号把该题滚到视窗顶部；
已答蓝色 / 未答灰色 / 当前题绿色描边）

**已实现**：学生作答（单选/多选/判断/填空/改错/问答 六种题型）、交卷与到点自动冻结、答题卡加密落盘（`.eea`）、
教师端批量解密校验（管理端「收卷判分」页 / `exam-builder verify-answer-sheet`）。

**已实现**：场次可按题库设置**平均难度区间**（只约束抽出的 N 题均值，不限制单题）、
自动判分（单选/多选/判断/填空/改错，按题型勾选；双击可改分；问答人工判）、
已判分/未判分五份 CSV 导出（含按学号排序、按题型分项的考生成绩表）、
收卷后按场次生成缺考名单（已交/未交/缺考/失败/名单外）、
打包后自动保存工程（不会写进考试包目录，含子目录）；下次启动为空白工程，
可点工具栏【打开上次工程】恢复。

**安全能力（v0.15.x）**：主密钥加密备份/恢复、工程口令加密（默认开启）、
哈希链审计日志（工具栏可校验）、每生 PIN 登录（可选，打包成功后选择 PIN 表 CSV 保存位置）、交卷后按包策略回看、
按大题出题（大题→部分→小题，固定题库/题数/精确难度，可上下调整顺序）、
答题卡收据（教师 Ed25519 签名、公钥可验签、需核对指纹、含包哈希，防收卷后改卡）；
签名时间令牌（可选，需自建服务；默认关闭）；
发布包 `SHA256SUMS.txt`；完整边界见 `docs/安全边界与威胁模型.md`。
服务端答题卡签名尚未实现。

**仍未实现**：代码题（`judge{entry,harness,tests}`）的自动判分——编译运行学生代码
有安全与公平风险，仍按 judge 材料人工判；判分只按题型勾选，暂不支持逐题开关。

> 完整的安全边界与"防谁/不防谁"见 `docs/安全边界与威胁模型.md`。
>
> 答题卡的安全性边界：密钥由 `K_manifest` 派生，而它嵌在学生端 exe 里，
> 因此加密只能**防误改、防明文**，不能防蓄意篡改。真正的防作弊仍需现场监考
> 或将来做在线提交。详见 `docs/教师使用手册.md`。

---

## 仓库结构

```
crates/
  common/         # 加密、密钥派生、容器格式、时间门禁、确定性抽题（两端共用）
  builder/        # exam-builder：题库 + 场次 → 自解压学生端
  client/         # exam-client：学生端单 exe（GUI + CLI 双模式）
admin/
  app/core/       # 纯逻辑：模型、状态、时间解析、题库 IO（**不依赖 Qt**）
  app/ui/         # PySide6 界面
  buildtools/     # PyInstaller 打包脚本
  tests/          # Python 测试
banks/demo/       # 演示题库（真题库不入库）
schemas/          # 题库/清单的 JSON Schema（编辑器补全用，由 pydantic 生成）
docs/题库JSON规范.md  # 题库格式规范与可直接导入的样例
examples/         # 可直接导入的示例题库（含图片题）
licenses/         # LGPL/GPL 全文（PySide6 合规）
tools/            # 辅助脚本（embed-image.py 等）
docs/             # 教师手册、图片教程、架构方案、发布说明
scripts/          # verify-embed.sh / preflight-release.sh / make-release-zip.sh
.github/workflows # CI 与 Release
```

**单一真相**：版本号只在 `VERSION`；对外文案集中在 `branding.json`；加密参数随包存于 header。

---

## 开发

前置：Rust（见 `rust-toolchain.toml`）、Python 3.11+、[uv](https://github.com/astral-sh/uv)。
下面的命令是 bash 语法（Windows 上用 Git Bash / MSYS2 执行）。

```bash
# Rust
cargo test --workspace                 # 加密 / 抽题 / 容器 / 门禁
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check

# 管理端（首次执行会建好 .venv 并装齐 GUI/打包依赖）
cd admin
uv sync --extra gui --extra build
uv run pytest -q                       # 模型 / 状态 / UI 联动
uv run ruff check . && uv run ruff format --check .

# 端到端（打包 → 嵌入密钥 → 解锁）
bash scripts/verify-embed.sh           # 端到端断言

# 发布前自检
bash scripts/preflight-release.sh      # 加 --full 连管理端打包一起查
```

本地跑通一次完整演示：

```bash
cargo build --release -p client -p builder
MASTER=$(./target/release/exam-builder.exe keygen)
./target/release/exam-builder.exe build \
  --manifest banks/demo/manifest.example.json --banks banks/ \
  --stub ./target/release/exam-client.exe --out StudentClient.exe --master "$MASTER" --force
./StudentClient.exe --student 2021001 --package StudentClient.exe --master "$MASTER"
```

---

## 版本与发布

1. 改 `VERSION`，同步 `Cargo.toml` 与 `admin/pyproject.toml`（**三处必须一致**）
2. 提交，打 tag `v<版本>`，推送 tag
3. `release` 工作流校验版本与 tag 一致 → 构建 → 跑 e2e → 发布资产

本地已装 `gh`/令牌时，也可用 `bash scripts/make-release-zip.sh` 手动生成同构发布包。

---

## 安全模型

**这是"防君子"的系统，不是密码学意义上的强防护。** 请在采用前理解以下边界。

| 保护什么 | 手段 |
|---|---|
| 题库明文不外泄 | 每场独立 `session_secret`，AES-256-GCM |
| 提前拿到卷子 | 包内没有明文题目，密钥靠学号派生 |
| 提前/延后解锁 | 三个公共 HTTPS 时间源取中位数，失败即拒绝（fail-closed） |
| 场次之间串卷 | 每场独立密钥与数据段，实测跨场解密失败 |

**明确的限制**：

- **学号是低熵数据**。拿到包的人可以离线枚举名单，Argon2id 参数（64 MiB / 3 轮）只是把成本抬到 10⁵ 个学号约 19 分钟量级，不是密码学保证。
- **包内嵌入的是 `K_manifest`，不是 master**。泄漏后攻击者能读到场次时间与人数，**读不到题目**。
- 有逆向能力且知道合法学号的人，可以在时间窗内解锁——这是设计使然，无法用纯客户端方案消除。
- 学生端必须有网（校时）；完全断网时会拒绝解锁。

---

## 许可证

[Apache License 2.0](LICENSE)。

## 致谢

- 创始人 / 仓库作者：[AlbusRiddle](https://github.com/Albusriddle)
- 部分创意支持：[Stone](https://github.com/stone6060)
- 精神支持：[湖南大学 Robot 工作坊](https://github.com/RobotWorkShop-BigHeroX)

如果这个项目对你有帮助，欢迎点一个 ⭐ Star。
