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

## 本机快速开始（Windows，链接器可装 D 盘）

```powershell
# 1. 装 MinGW 链接器（约 300MB，可指定 D 盘路径）
winget install --id MartinStorsjo.LLVM-MinGW -e --location D:\tools\llvm-mingw
$env:Path = "D:\tools\llvm-mingw\bin;$env:Path"

# 2. 加 gnu target（.cargo/config.toml 已配好 linker）
rustup target add x86_64-pc-windows-gnu

# 3. 跑测试（加密往返 / 确定性抽题 / 容器打包解包）
cargo test --workspace --target x86_64-pc-windows-gnu
```

> 发布给学生机房的正式版请切 MSVC target（需 VS Build Tools，见 docs 方案 §6.1）。

## 端到端演示（M2 目标）

```powershell
# 生成主密钥（请自行保管，勿提交）
$master = cargo run -p builder -- keygen

# 用 demo 题库 + 示例清单打包（先编译出 client 作为 stub）
cargo build -p client --release --target x86_64-pc-windows-gnu
cargo run -p builder -- build `
  --manifest banks/demo/manifest.example.json `
  --banks banks/ `
  --stub target/x86_64-pc-windows-gnu/release/exam-client.exe `
  --out StudentClient.exe `
  --master $master

# 学生端解锁 + 抽题（骨架版手动传学号/场次/密钥）
./StudentClient.exe --student 2021001 --session 1 --master $master --package StudentClient.exe
```

## 安全红线

- **真题库、密钥绝不入库**：`banks/`（除 demo）、`*.key`、`secrets/` 已在 .gitignore。
- 主密钥用 `exam-builder keygen` 生成后立即存入密码管理器。
- 仓库私有；CI 已配 gitleaks 扫密钥。
