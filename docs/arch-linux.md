# Arch Linux：原生构建与使用

> 适用版本：**v1.10.0**。Linux 原生运行、AppImage 与示例题库默认加载**已包含在 v1.10.0 中**，
> 属于该版本正式发布的能力。但 Linux 产物**不随 GitHub Release 分发**：请从源码构建，
> 或从 CI 的 `linux-appimage` 制品获取（见第 3 节）。

教师端（`ExamAdmin`）、学生端（`exam-client`）和可选的局域网网页服务端（`exam-server`）都能在 Linux 原生运行，不需要 Wine。请按**学生实际使用的系统**准备考试包：Linux 的 ELF / AppImage 不能发给 Windows 学生，Windows 的 `.exe` 也不能在 Linux 原生运行；打包时不会交叉编译学生端模板。

## 1. 安装依赖

Arch 不支持部分升级；安装依赖时先做完整系统升级：

```bash
sudo pacman -Syu --needed \
  base-devel git rustup uv python curl pkgconf zip unzip squashfs-tools \
  wayland libxkbcommon libxkbcommon-x11 libxcb \
  xcb-util-cursor xcb-util-keysyms xcb-util-wm \
  libxrandr libxi libxcursor libxrender libxext libice libsm \
  mesa libglvnd fontconfig noto-fonts-cjk dbus
rustup toolchain install stable --profile minimal
rustup component add --toolchain stable rustfmt clippy
```

- 仓库的 `rust-toolchain.toml` 已选定 stable，不要改全局默认工具链；
- `curl` 和系统 CA 证书不只是构建依赖，**学生端解锁取时也需要**；Arch 的 `curl` 包会带证书依赖，请保持系统证书更新；
- 脚本用 `uv` 管理 Python 3.13，必要时自动下载解释器，避免 Arch 滚动升级 Python 的速度超过 PySide6 / PyInstaller 的支持进度。依赖装在 `admin/.venv`，**不要**用 `sudo pip` 或 `--break-system-packages`；首次准备需要联网。

## 2. 从源码构建与启动

在仓库根目录执行：

```bash
bash scripts/build-linux.sh
bash scripts/run-admin.sh
```

构建会在 `target/release/` 生成原生程序、准备管理端 Python 环境，并执行依赖、示例题库与加密自检。启动脚本不要求当前目录是仓库根，也会转发参数：

```bash
bash scripts/run-admin.sh --version
bash scripts/run-admin.sh --selftest
```

也可以手动执行同样的步骤：

```bash
cargo build --locked --release --workspace
cd admin
uv sync --locked --python 3.13 --extra gui --extra build --group dev
uv run --locked python -m app
```

新工程默认加载 `banks/demo/` 里的数学、英语和题型展示三个示例库，共 **3 库 14 题**，不带学生和场次。示例只用于演练，正式考试请移除示例再导入自己的题库；它与包内 `示例题库/` 是另一套示例（内置默认 3 库 14 题，包内 `示例题库/` 3 库 11 题，`exam_id` 不同），不要同时导入。重新打开已保存的工程时，不会额外加载示例。

源码运行默认的学生端输出是 `dist/StudentClient`（无 `.exe`），模板是 `target/release/exam-client`。请通过图形界面生成主密钥、编排场次并打包。学生拿到的是 **已打包的文件**，不是裸模板：

```bash
chmod +x ./StudentClient    # 下载或复制后若丢失执行权限
./StudentClient
```

学生解锁仍需至少两个 HTTPS 时间源成功且时间一致性达标；解锁后离线作答。Linux 分发没有改变时间门禁、加密方式，也没有消除 [已接受的风险](风险接受记录.md)。

## 3. AppImage 桌面版（推荐分发方式）

**GitHub Release 不发布 Linux 产物。** 获取方式只有两个：

1. 在 Linux 上从源码构建（推荐，见下）；
2. 从 GitHub Actions 的 `linux-appimage` job 下载 `exam-admin-linux-appimage` 制品（按工作流保留期提供，不是正式 Release 资产）。

```bash
bash scripts/build-linux.sh --appimage
```

产物：

| 文件 | 用途 |
|---|---|
| `dist/ExamAdmin-<版本>-x86_64.AppImage` | 教师桌面程序 |
| `dist/stub-template-x86_64.AppImage` | 学生端模板，**不是考试包**，不能直接发给学生 |
| `dist/ExamAdmin-<版本>-linux.zip` | 传统目录式教师包；用 `--appimage` 构建时也包含学生 AppImage 模板 |

### 3.1 教师与学生的操作

1. 把教师 AppImage 放到固定目录，在文件管理器的「属性 → 权限」里允许执行后双击；也可以在终端 `chmod +x ExamAdmin-*.AppImage` 再运行。
2. 可选：点「帮助 → 创建桌面启动器…」把程序加进应用菜单。它只写当前用户目录，不需要 `sudo`，也不会在启动时自动创建。移动 AppImage 后要再创建一次，更新启动器记录的绝对路径。
3. 和 Windows 一样依次使用「题库 → 学生 → 场次 → 打包 → 收卷判分」。示例库可用于演练，只有明确选入场次的题库才用于该场考试。
4. AppImage 管理端默认选 `stub-template.AppImage`，默认输出 `StudentClient.AppImage`。不必在每台打包机上安装 AppImage 构建工具：Rust builder 只把现成格式的加密考试数据追加到预建模板。
5. **只分发最终生成的学生 AppImage。** 不要分发管理端、主密钥、工程文件或整张 PIN 表。学生允许执行后双击，输入学号和（启用时）本人 PIN；考后回收加密的 `.eea` 答题卡。

教师 AppImage 自带 Python、Qt、builder、网页服务端、学生模板以及 Noto CJK 常规与粗体字库；学生 AppImage 自带原生 GUI 和字体。两端都不需要另外安装 Python 或 Wine。Linux 管理端使用 Fusion 样式，Windows 保持原生样式。

AppImage 内部目录是只读的。学生端从**外层 AppImage 文件**读取考试数据，可写时把答题卡存在外层文件旁；管理端可写时同样把工程存在外层文件旁。不可写时回退到用户目录，不会把持久文件写进临时挂载点或解压目录。

### 3.2 运行条件与兼容性

需要 Linux x86_64 图形会话、可用的 X11 或 Wayland/OpenGL 驱动、`curl` 与系统 CA 证书。 **Arch 本机构建不等于能在旧发行版运行。** `linux-appimage` job 使用 Ubuntu 22.04（glibc 2.35）作为较旧的构建基线；考试前仍须在目标发行版上试跑，不能把兼容性推断当成验收结果。

AppImage runtime 固定在上游发布 `20251108`，只使用未超限且 SHA-256 校验正确的下载文件。它自带 FUSE 支持，不需要 `libfuse.so.2`，但双击运行通常要求能访问 `/dev/fuse`。受限容器或不支持挂载的桌面可以解压运行：

```bash
./ExamAdmin-<版本>-x86_64.AppImage --appimage-extract-and-run
./StudentClient.AppImage --appimage-extract-and-run
```

应用菜单启动器走常规 FUSE 路径，CI 走解压运行；两种方式应使用同一份外层考试数据与持久化路径。许可证在 AppImage 的 `usr/share/doc/EncryptedExamSystem/`，目录式包也附有许可证。

### 3.3 传统目录式管理端

不需要 AppImage 时可以只构建 Linux ZIP：

```bash
bash scripts/build-linux.sh --package
```

解压 `dist/ExamAdmin-<版本>-linux.zip`，进入 `ExamAdmin/` 运行 `./ExamAdmin`；整个目录要一起保留，特别是 `_internal/`。原生学生模板是旁边的 `stub-template`。这种分发方式依赖目标机的系统库与 glibc；裸 ELF 学生端还需要系统 CJK 字体。不要把 Windows 与 Linux 的发布目录混用。

## 4. Wayland、X11 与字体

Rust 学生端支持 Wayland 和 X11；Qt 通常会自动选择。缺 Qt Wayland 插件或驱动不兼容时可用 XWayland：

```bash
sudo pacman -S --needed xorg-xwayland
QT_QPA_PLATFORM=xcb bash scripts/run-admin.sh
```

也可以强制 Qt 走原生 Wayland：

```bash
QT_QPA_PLATFORM=wayland bash scripts/run-admin.sh
```

请从图形会话启动，不要只从 TTY 或无图形的 SSH 启动。`QT_QPA_PLATFORM=offscreen` 只用于测试。

裸 ELF 学生端会在 Arch 上查找 `/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc` 及同目录的粗体字库；中文显示方框时，安装 `noto-fonts-cjk` 后重启。Qt 插件加载失败可开 `QT_DEBUG_PLUGINS=1` 看日志，或用 `ldd` 查找 `not found` 的系统库。AppImage 字体验收看 `--dump-ui` 输出里的 `bundled_fonts` 实际注册结果；只看到字体家族同名不能证明字体随包。

## 5. 用户文件与旧设置迁移

| 文件 | Linux 位置 |
|---|---|
| 教师设置、收据密钥、默认源码模式工程 | `${XDG_CONFIG_HOME:-$HOME/.config}/ExamSystem/` |
| 教师崩溃日志 | `${XDG_STATE_HOME:-$HOME/.local/state}/ExamSystem/admin-crash.log` |
| 打包版示例题库快照 | `${XDG_DATA_HOME:-$HOME/.local/share}/ExamSystem/demo-banks/<内容哈希>/` |
| 学生欢迎提示的「不再提示」标记 | `${XDG_DATA_HOME:-$HOME/.local/share}/ExamSystem/skip_welcome` |
| 学生答题卡 | `StudentClient` / 外层学生 AppImage 同目录；不可写时用 `${XDG_DATA_HOME:-$HOME/.local/share}/ExamSystem/answers/` |

管理端只接受绝对 XDG 路径；学生端也请使用绝对路径。教师打包版优先把工程存到程序旁的 `projects/` （AppImage 使用外层文件的位置）。已显式配置 `LOCALAPPDATA` 或 `APPDATA` 的便携式管理端继续使用原位置；要用 XDG 就先取消这些覆盖。

打包版启动时把公开默认示例保存为按内容区分的快照，工程引用该持久路径。升级后新增的题库不会覆盖旧工程引用的副本；只要工程还在用，就不要删除快照。只读的 `--selftest` 从原始资源校验，**不创建快照**，也不写用户目录。源码版直接引用 `banks/demo/`。

### 5.1 迁移旧设置

旧 Linux 源码版使用 `~/ExamSystem/settings.ini`。如需保留欢迎提示偏好、上次工程、工程目录和加密偏好，请退出管理端，**在第一次启动新版之前**手动复制。下面命令保留旧文件，也不会覆盖已有的 XDG 配置：

```bash
case "${XDG_CONFIG_HOME:-}" in
  /*) config_root="$XDG_CONFIG_HOME" ;;
  *) config_root="$HOME/.config" ;;
esac
settings_dir="$config_root/ExamSystem"
if [ -f "$HOME/ExamSystem/settings.ini" ] && [ ! -e "$settings_dir/settings.ini" ]; then
  mkdir -p -- "$settings_dir"
  cp -p -- "$HOME/ExamSystem/settings.ini" "$settings_dir/settings.ini"
fi
```

目标文件已存在时请保留它，通过界面重新选择所需设置。学生端沿用 `~/ExamSystem/skip_welcome` 的一次性自动迁移；旧 `.eea` 格式不变，去原保存位置收集即可。Windows 设置路径不变。

请把私有工程与密钥放在确实支持 `0600` 权限的 Linux 文件系统上；FAT/exFAT 和部分 NTFS 挂载无法落实这个权限。`chmod +x` 后仍报 `Permission denied` 时，还要检查挂载是否 `noexec`，必要时移到允许执行的本地目录。

## 6. 维护者验证

```bash
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
bash scripts/run-admin-tests.sh
(cd admin && uv run --locked ruff check . && uv run --locked ruff format --check .)
bash scripts/verify-embed.sh
bash scripts/preflight-release.sh
bash scripts/smoke-web.sh
# 构建 AppImage 后：真实时间门禁、外层考试数据与拒绝路径
admin/.venv/bin/python scripts/smoke-appimage.py --native
```

端到端脚本会访问真实 HTTPS 时间源；网络受限时必须**拒绝解锁**，不能跳过门禁。 `linux-appimage` job 构建真实产物、以无 FUSE 方式跑学生端烟测，并用教师端 `--selftest` 与 `--dump-ui` 验证「自检不写用户目录、随包字体确实注册、示例快照在解压进程退出后仍可读」。Windows 由 `admin-package` 另行测构建与测试。构建或上传 CI 制品**不会**创建 tag 或 Release。

在图形会话中还可以做学生端 GUI / 答题卡烟测：

```bash
cargo build --locked -p client
bash scripts/smoke-answers.sh
```

脚本会短暂打开 debug 学生端，填入合成答案并用 release builder 校验加密答题卡。调试开关在 release 构建中无效，**不要分发** `target/smoke-answers/` 下的调试包。布局以程序自报的 rect、`contains`、 `center_offset` 为准，不用屏幕截图判断。
