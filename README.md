# EncryptedExamSystem · 保密考试系统

> 管理端编排多场考试 → 打包加密题库 → 生成"学生端"自解压 exe →
> 学生凭**学号 + 网络时间门禁**解锁 → 离线确定性抽题生成个人试卷。

完整架构与安全威胁模型见 [docs/保密考试系统_架构与实施方案.md](docs/保密考试系统_架构与实施方案.md)。

## 仓库结构

```
crates/
  common/    # 加密、密钥派生、确定性抽题、容器格式（两端共用，黄金测试在这）
  builder/   # exam-builder CLI：题库+场次 → 自解压学生端
  client/    # exam-client：学生端单 exe（骨架版 CLI，M4 换 egui）
admin/       # PySide6 管理端（M3）
worker/      # Cloudflare Worker 签名时间服务（M4）
banks/demo/  # 假题库，供 CI 与本地调试（真题库不入库，见 .gitignore）
```

## 本机快速开始（Windows）

本机已装 VS Build Tools，直接用默认 MSVC target（兼容性最好）：

```powershell
cargo test --workspace          # 15 个单元测试：加密/抽题/容器
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

# 3. 学生端解锁 + 确定性抽题（骨架版手动传参，M4 换 GUI + 时间门禁）
.\StudentClient.exe --student 2021001 --session 1 --master $master --package StudentClient.exe
```

已验证：名单内学生解锁抽题成功、名单外拒绝、重复运行试卷指纹一致。

## 安全红线

- **真题库、密钥绝不入库**：`banks/`（除 demo）、`*.key`、`secrets/` 已在 .gitignore。
- 主密钥用 `exam-builder keygen` 生成后立即存入密码管理器。
- 仓库私有；CI 已配 gitleaks 扫密钥。
