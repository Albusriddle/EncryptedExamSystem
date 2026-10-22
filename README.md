# EncryptedExamSystem · 保密考试系统

> 管理端编排多场考试 → 打包加密题库 → 生成"学生端"自解压 exe →
> 学生凭**学号 + 网络时间门禁**解锁 → 离线确定性抽题生成个人试卷。

**如果你是老师**：直接看 [docs/教师使用手册.md](docs/教师使用手册.md)，不需要懂编程。

**如果你要了解设计**：见 [docs/保密考试系统_架构与实施方案.md](docs/保密考试系统_架构与实施方案.md)。

## 交付物

| 产物 | 给谁 | 说明 |
|---|---|---|
| `ExamAdmin`（文件夹） | 教师 | 图形管理端，解压双击即用 |
| 学生端 `.exe` | 学生 | 教师打包生成，双击输入学号即可 |
| `exam-builder.exe` | 进阶用户 | 命令行打包工具 |

## 仓库结构

```
crates/
  common/    # 加密、密钥派生、确定性抽题、容器格式（两端共用，黄金测试在这）
  builder/   # exam-builder CLI：题库+场次 → 自解压学生端
  client/    # exam-client：学生端单 exe（GUI + CLI 双模式）
admin/       # PySide6 管理端（四 Tab：题库/学生/场次/打包）
  packaging/ # PyInstaller 打包脚本，产出 ExamAdmin 文件夹
worker/      # Cloudflare Worker 签名时间服务（M4，未启用）
banks/demo/  # 假题库，供 CI 与本地调试（真题库不入库，见 .gitignore）
```

## 本机快速开始（Windows）

本机已装 VS Build Tools，直接用默认 MSVC target（兼容性最好）：

```powershell
cargo test --workspace          # 79 个测试：加密/抽题/容器/门禁/GUI
```

若换一台 C 盘空间紧张的机器，可改走 GNU target + MinGW（装 D 盘），见 docs 方案 §6.1。

## 端到端演示（M2 已验证通过）

```powershell
cargo build --release -p client -p builder

# 1. 生成主密钥（自行保管，勿提交）
$master = .\target\release\exam-builder.exe keygen

# 2. 打包：demo 题库 + 示例清单 → 自解压学生端
.\target\release\exam-builder.exe build `
  --manifest banks/demo/manifest.example.json --banks banks/ `
  --stub target/release/exam-client.exe --out StudentClient.exe --master $master

# 3. 学生端解锁 + 确定性抽题
.\StudentClient.exe --student 2021001 --master $master --package StudentClient.exe
```

## 密钥嵌入（学生免输密码）

发布给学生的包可以**不含任何密钥输入**：把 `K_manifest` 嵌入学生端即可。

```powershell
# 1. 打包时额外导出 K_manifest
.\target\release\exam-builder.exe build `
  --manifest banks/demo/manifest.example.json --banks banks/ `
  --stub target/release/exam-client.exe --out /tmp/x.exe --master $master `
  --emit-keyfile kf.txt

# 2. 用该 keyfile 重新编译学生端（密钥被分片混淆后嵌入）
$env:EXAM_KEYFILE = "kf.txt"
cargo build --release -p client

# 3. 用新的 stub 打包，学生端即可免输密钥
.\target\release\exam-builder.exe build `
  --manifest banks/demo/manifest.example.json --banks banks/ `
  --stub target/release/exam-client.exe --out StudentClient.exe --master $master
.\StudentClient.exe --student 2021001 --package StudentClient.exe
```

**安全边界（必须理解）**：嵌入的是 `K_manifest` 而**不是 master**。
泄漏后果是攻击者可读到场次时间与人数，但**拿不到任何题目**
（题库用每场独立的 `session_secret` 加密，仍需猜中学号才能触发 Argon2id）。

一键验收：`bash scripts/verify-embed.sh`（CI 已接入）

## 安全红线

- **真题库、密钥绝不入库**：`banks/`（除 demo）、`*.key`、`secrets/` 已在 .gitignore。
- 主密钥用 `exam-builder keygen` 生成后立即存入密码管理器。
- 仓库私有；CI 已配 gitleaks 扫密钥。
