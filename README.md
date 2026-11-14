# 保密考试系统（EncryptedExamSystem）

> 一套面向校内闭卷/随堂考试的离线加密考试系统：教师端编排题目与场次，生成学生端 exe；学生在断网环境作答，答题卡加密落盘；教师端收卷、校验、判分并导出成绩。

当前版本：**1.2.x**

- 教师端：`ExamAdmin`（PySide6 图形界面）
- 命令行工具：`exam-builder`（打包、校验、导出 PIN、考后查看原卷）
- 学生端：`exam-client`（双击运行，按场次与网络时间解锁）

> 本系统是“防误操作 + 现场监考”工具，不是密码学意义上的强防作弊系统。  
> 安全边界与已接受风险见 [安全边界与威胁模型](docs/安全边界与威胁模型.md) 与 [风险接受记录](docs/风险接受记录.md)。

---

## 1. 主要能力

### 考试与题型

- 单选、多选、判断、填空、改错、问答、**代码简答**七种题型；
- 代码简答题支持：
  - 等宽多行输入；
  - 行号；
  - 按 `code_language` 使用 syntect 做 **python / c / cpp / java / rust / javascript / typescript / go / sql / bash / text** 语法高亮；
  - 答案保留缩进、换行、行尾空格；单题上限 1 MiB，整卡答案上限 16 MiB；
  - 不执行学生代码，不自动判分，由教师人工评分；
- 问答/代码简答之外的选择、判断、填空、改错可自动判分，支持人工改分；
- 场次可按题库设置平均难度区间，支持“大题 → 部分 → 小题”。

### 考试组织

- 教师端编排题库、学生名单、班级、场次；
- 生成学生端 exe，可选每生 6 位 PIN 登录；
- 网络时间门禁：三个公共 HTTPS 时间源中位数，失败即拒绝；
- 可选签名时间令牌（默认关闭，需自建时间服务）；
- 交卷与到点自动冻结；
- 收卷后批量校验答题卡、生成缺考名单、导出 CSV。

### 回看与原卷

- 默认「仅教师可查看原卷」：学生端无回看入口；
- 取消该限制后，学生已交卷且未到结束时间可只读回看；回看页可返回已交卷页；
- 教师考后查看原卷使用 `exam-builder review-paper`，需要教师电脑上的 master，不经过学生端时间门禁；
- 学生端不接收、不保存教师 master。

### 安全与审计

- 答题卡加密落盘（`.eea`），不存明文答案；
- 主密钥加密备份 / 恢复，工程文件口令加密；
- 哈希链审计日志；
- 答题卡收据（教师侧 Ed25519，可检测收卷后改动，不防伪造）；
- 发布包带 `SHA256SUMS.txt`。

---

## 2. 学生端答题卡与代码题

学生答题卡保存为：

```text
answers-<学号>-<场次>-<试卷指纹前8位>.eea
```

答题卡是加密文件，教师端用 master 解密校验。代码题答案保存在：

```json
"answers": [
  "def f():\n\treturn 1  \n"
]
```

代码内容不做 `trim`、不替换缩进、不截断；解密后的 JSON 可直接提取：

```bash
exam-builder verify-answer-sheet answers-2021001-1.eea \
  --package 学生端.exe --master <master> --json > result.json
```

```python
import json

data = json.load(open("result.json", encoding="utf-8"))
code = data["sheet"]["answers"][0]
open("answer.py", "w", encoding="utf-8", newline="").write(code)
```

> 代码题需要 **v1.1.0+** 学生端；行号与语法高亮需要 **v1.2.0+**。  
> 旧学生端会忽略 `code_language`，仍可作答，但没有高亮。

---

## 3. 快速开始（教师）

1. 从 Release 下载 `ExamAdmin-<版本>-windows.zip`；
2. 解压，进入 `ExamAdmin` 目录，双击 `ExamAdmin.exe`；
3. 在「题库」页导入题库（原生 JSON 或题目/答案分离格式）；
4. 在「学生」页维护名单 / 班级；
5. 在「场次」页创建场次，选择题库、抽题数、时间、名单；
6. 在「打包」页：
   - 生成/粘贴主密钥；
   - 选择学生端 stub；
   - 选择输出 exe；
   - 按需启用 PIN、时间令牌、学生回看；
   - 开始打包；
7. 学生端 exe 发给学生；
8. 收卷后进入「收卷判分」页，选择学生端包、粘贴 master，批量校验答题卡；
9. 生成成绩，人工给问答/代码题打分，导出 CSV。

---

## 4. 快速开始（学生）

- 双击学生端 exe；
- 输入学号（启用 PIN 时再输入 6 位 PIN）；
- 等待网络时间校验；
- 作答，交卷；
- 如教师允许，可在结束前只读回看；
- 答题卡自动加密保存在 exe 同目录（不可写时回退 `%LOCALAPPDATA%`）。

---

## 5. 从源码构建

前置：

- Rust（见 `rust-toolchain.toml`）
- Python 3.11+
- [uv](https://github.com/astral-sh/uv)
- Windows 管理端需要 PySide6 / PyInstaller

```bash
# Rust：common / builder / client
cargo build --release --workspace

# 管理端
cd admin
uv sync --extra gui --group dev
uv run python -m app
```

生成管理端发布目录：

```bash
cd admin
uv run python buildtools/build_exe.py
```

发布前检查：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd admin && uv run pytest -q
uv run ruff check . && uv run ruff format --check .
cd .. && bash scripts/verify-embed.sh
bash scripts/preflight-release.sh
```

---

## 6. 目录结构

```text
crates/common/       共享模型、加密、容器、答题卡、时间门禁
crates/builder/      exam-builder：打包、校验、review-paper、PIN 导出
crates/client/       学生端：GUI、解锁流程、答题卡、代码编辑器
admin/               教师端 PySide6 + 打包工具
banks/               示例题库
examples/            可直接导入的示例
docs/                教师手册、题库规范、安全边界、时间令牌协议、发布说明
schemas/             题库 / manifest JSON Schema
scripts/             verify-embed、preflight、release_body、烟测、管理端测试
licenses/            LGPL / GPL 全文
```

---

## 7. 安全边界（必读）

### 已实现

- 考试包不含明文标准答案；
- 答题卡不是明文；
- 工程文件默认口令加密；
- 主密钥只通过环境变量/文件传给教师工具，不进入学生端命令行；
- 学生端不保存 master；
- 时间门禁失败即拒绝；
- 代码高亮只影响显示，不执行代码。

### 已接受风险

项目明确接受以下风险，不在当前 v1.x 内实现 P0 新格式（v9）：

- 同场学生可解出本场全部题干；
- 同场学生可读取/伪造他人答题卡；
- 默认无 PIN 时，任意已知名单内学号可解锁；
- Header 可被重加密，时间门禁只是 UI 约束。

完整决策与补偿措施见 [风险接受记录](docs/风险接受记录.md)。

### 版本门禁

- 普通包：容器 v4；
- 时间令牌：v5；
- 大题：v6；
- 大题部分：v7；
- 含代码简答题：v8，旧学生端会明确拒绝，需 v1.1.0+ 学生端；
- P0 的 per-student/Header 签名方案记为 v9，当前不实现。

---

## 8. 文档

- [教师使用手册](docs/教师使用手册.md)
- [题库 JSON 规范](docs/题库JSON规范.md)
- [安全边界与威胁模型](docs/安全边界与威胁模型.md)
- [风险接受记录](docs/风险接受记录.md)
- [时间令牌协议](docs/时间令牌协议.md)
- [发布说明](docs/release-notes.md)
- [代码审阅报告](docs/代码审阅报告.md)

---

## 9. 许可与致谢

本项目代码以 [Apache License 2.0](LICENSE) 发布。第三方组件许可见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)。

- 创始人 / 仓库作者：[AlbusRiddle](https://github.com/Albusriddle)
- 部分创意支持：[Stone](https://github.com/stone6060)
- 精神支持：[湖南大学 Robot 工作坊](https://github.com/RobotWorkShop-BigHeroX)
