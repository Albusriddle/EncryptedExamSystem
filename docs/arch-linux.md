# Arch Linux：原生构建与使用

教师端（`ExamAdmin`）、学生端（`exam-client`）和可选的局域网网页服务端（`exam-server`）都可以在 Linux 原生运行，无需 Wine。请按学生使用的系统准备考试包：Linux 的 ELF 或 AppImage 不能发给 Windows 学生，Windows 的 `.exe` 也不能在 Linux 原生运行。打包时不会交叉编译学生端模板。

## 1. 安装依赖

Arch 不支持部分升级；安装依赖时请做完整系统升级：

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

仓库的 `rust-toolchain.toml` 已选定 stable，不用改全局默认工具链。`curl` 和系统 CA 证书不只构建时需要，**学生解锁时也需要**；Arch 的 `curl` 包会安装证书依赖，请保持系统证书更新。

脚本用 `uv` 管理 Python 3.13，必要时下载解释器，避免 Arch 升级 Python 的速度超过 PySide6 / PyInstaller 的支持进度。依赖放在 `admin/.venv`，不要用 `sudo pip` 或 `--break-system-packages`。首次准备需要联网。

## 2. 从源码构建与启动

在仓库根目录执行：

```bash
bash scripts/build-linux.sh
bash scripts/run-admin.sh
```

构建会在 `target/release/` 生成原生程序，准备管理端 Python 环境，并执行依赖、示例题库及加密自检。启动脚本不要求当前工作目录是仓库根，还会转发参数：

```bash
bash scripts/run-admin.sh --version
bash scripts/run-admin.sh --selftest
```

也可以手动执行：

```bash
cargo build --locked --release --workspace
cd admin
uv sync --locked --python 3.13 --extra gui --extra build --group dev
uv run --locked python -m app
```

新工程默认加载 `banks/demo/` 的数学、英语和题型展示，共 **3 库 14 题**，不带学生和场次。示例只用于演练，正式考试请移除示例，再导入自己的题库。重新打开已保存的工程时，不会额外加载示例。

源码运行的默认学生端输出是 `dist/StudentClient`（无 `.exe`），模板是 `target/release/exam-client`。请通过图形界面生成主密钥、编排场次并打包。学生拿到的是**已打包的文件**，不是裸模板：

```bash
chmod +x ./StudentClient    # 下载或复制后若丢失执行权限
./StudentClient
```

学生解锁仍需至少两个 HTTPS 时间源成功且时间一致性达标；解锁后离线作答。Linux 分发没有改变时间门禁、加密方式或 [已接受的风险](风险接受记录.md)。

## 3. 推荐：AppImage 桌面版

**GitHub Release 不随本改动分发 Linux 产物。** 请从源码构建，或从 GitHub Actions 的 `linux-appimage` job 下载 `exam-admin-linux-appimage` 产物（按工作流保留期提供）。构建命令：

```bash
bash scripts/build-linux.sh --appimage
```

产物：

| 文件 | 用途 |
|---|---|
| `dist/ExamAdmin-<版本>-x86_64.AppImage` | 教师桌面程序 |
| `dist/stub-template-x86_64.AppImage` | 学生端模板，**不是考试包** |
| `dist/ExamAdmin-<版本>-linux.zip` | 传统目录式教师包；使用 `--appimage` 构建时也包含学生 AppImage 模板 |

### 3.1 教师与学生的操作

1. 将教师 AppImage 放到固定目录，在文件管理器的「属性 → 权限」允许执行后双击。也可在终端用 `chmod +x ExamAdmin-*.AppImage`，再运行对应文件。
2. 可选：点「帮助 → 创建桌面启动器…」，把程序添加到应用菜单。只写入当前用户目录，不需要 `sudo`，也不会在启动时自动创建。移动 AppImage 后请再次创建，更新启动器记录的绝对路径。
3. 跟 Windows 一样，依次使用「题库 → 学生 → 场次 → 打包 → 收卷判分」。示例库可用于演练，只有明确选入场次的题库才用于该场考试。
4. AppImage 管理端默认选 `stub-template.AppImage`，默认输出 `StudentClient.AppImage`。不必为每场考试安装 AppImage 构建工具；Rust builder 会把现有格式的加密考试数据追加到预建模板。
5. **只发生成后的学生 AppImage**。不要分发管理端、主密钥、工程文件或整张 PIN 表。学生允许执行后双击，输入学号和（启用时）本人 PIN。考后回收加密的 `.eea` 答题卡。

教师 AppImage 自带 Python、Qt、builder、网页服务端、学生模板及 Noto CJK 常规和粗体字库；学生 AppImage 自带原生 GUI 和字体。两端都不要求另外安装 Python 或 Wine。Linux 管理端用 Fusion 样式，Windows 保持原生样式。

AppImage 内部目录是只读的。学生端从**外层 AppImage 文件**读取考试数据，可写时把答题卡存到外层文件旁；管理端可写时同样把工程存到外层文件旁。不可写时走用户目录回退，不会把持久文件存进临时挂载点或解压目录。

### 3.2 运行条件与兼容性

需要 Linux x86_64 图形会话、可用的 X11 或 Wayland/OpenGL 驱动、`curl` 和系统 CA 证书。Arch 本机构建不等于可在旧发行版上运行。`linux-appimage` job 用 Ubuntu 22.04（glibc 2.35）作为较旧的构建基线；考试前仍须在目标发行版上试跑，不能把兼容性推断为验收结果。

AppImage runtime 固定在上游发布 `20251108`，只使用未超限且 SHA-256 校验正确的下载文件。runtime 自带 FUSE 支持，不需要 `libfuse.so.2`，但直接双击通常需要能访问 `/dev/fuse`。受限容器或不支持挂载的桌面可用解压运行：

```bash
./ExamAdmin-<版本>-x86_64.AppImage --appimage-extract-and-run
./StudentClient.AppImage --appimage-extract-and-run
```

应用菜单启动器走常规 FUSE 路径，CI 走解压运行；两种方式应使用同一份外层考试数据和持久化路径。许可证在 AppImage 的 `usr/share/doc/EncryptedExamSystem/`，目录式包也附有许可证。需检查或更换 Qt / 字体时，可用 `--appimage-extract` 解压到 `squashfs-root`，再依据原许可、用随仓库提供的脚本重新构建。

### 3.3 传统目录式管理端

不需要 AppImage 时可以只构建 Linux ZIP：

```bash
bash scripts/build-linux.sh --package
```

解压 `dist/ExamAdmin-<版本>-linux.zip`，进 `ExamAdmin/` 运行 `./ExamAdmin`；整个目录要一起保留，特别是 `_internal/`。原生学生模板为旁边的 `stub-template`。这种分发方式依赖目标机的系统库和 glibc；裸 ELF 学生端还需要系统 CJK 字体。不要把 Windows 和 Linux 的发布目录混用。

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

请从图形会话启动，不要只从 TTY 或无图形的 SSH 启动。`QT_QPA_PLATFORM=offscreen` 仅用于测试。

裸 ELF 学生端会在 Arch 上查找 `/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc` 及同目录的粗体字库。中文显示方框时，安装 `noto-fonts-cjk` 后重启。若 Qt 插件加载失败，可开 `QT_DEBUG_PLUGINS=1` 查看日志，或用 `ldd` 查找 `not found` 的系统库。AppImage 字体验收看 `--dump-ui` 输出中 `bundled_fonts` 的实际注册结果；仅看到 `font_family` 同名不能证明字体随包。

## 5. 用户文件与旧设置迁移

| 文件 | Linux 位置 |
|---|---|
| 教师设置、收据密钥、默认源码模式工程 | `${XDG_CONFIG_HOME:-$HOME/.config}/ExamSystem/` |
| 教师崩溃日志 | `${XDG_STATE_HOME:-$HOME/.local/state}/ExamSystem/admin-crash.log` |
| 打包版示例题库快照 | `${XDG_DATA_HOME:-$HOME/.local/share}/ExamSystem/demo-banks/<内容哈希>/` |
| 学生欢迎提示的「不再提示」标记 | `${XDG_DATA_HOME:-$HOME/.local/share}/ExamSystem/skip_welcome` |
| 学生答题卡 | `StudentClient` / 外层学生 AppImage 同目录；不可写时用 `${XDG_DATA_HOME:-$HOME/.local/share}/ExamSystem/answers/` |

管理端只接受绝对 XDG 路径；学生端也请使用绝对路径。教师打包版优先把工程存到程序旁的 `projects/`（AppImage 使用外层文件的位置）。已显式配置 `LOCALAPPDATA` 或 `APPDATA` 的便携式管理端继续使用原位置；若要用 XDG，请取消这些覆盖。

打包版启动时把公开默认示例保存为按内容区分的快照，工程引用该持久路径。升级后的新题库不会覆盖旧工程的来源；只要工程仍在用，就不要删除快照。只读 `--selftest` 会从原始资源校验，不创建快照。源码版直接引用 `banks/demo/`。

### 迁移设置

旧 Linux 源码版使用 `~/ExamSystem/settings.ini`。如需保留欢迎提示偏好、上次工程、工程目录和加密偏好，请退出管理端，**在第一次启动新版前**手动复制。以下命令保留旧文件，也不会覆盖已有的 XDG 配置：

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

目标文件若已存在，请保留它，通过界面重新选所需设置。学生端沿用 `~/ExamSystem/skip_welcome` 的一次性自动迁移。旧 `.eea` 格式不变，去原保存位置收集即可。Windows 设置路径不变。

请将私有工程与密钥放在确实支持 `0600` 权限的 Linux 文件系统上；FAT/exFAT 和部分 NTFS 挂载无法落实这个权限。`chmod +x` 后仍报 `Permission denied` 时，还要检查挂载是否为 `noexec`，必要时移到允许执行的本地目录。

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

端到端脚本会访问真实 HTTPS 时间源，网络受限时必须拒绝解锁，不能跳过门禁。`linux-appimage` job 构建真实产物、以无 FUSE 方式跑学生端烟测，并用教师端 `--selftest` 和 `--dump-ui` 验证自检不写用户目录、随包字体确实注册、示例快照在解压进程退出后仍可读。Windows 由 `admin-package` 另行测构建与测试。构建或上传 CI 产物不会创建 tag 或 Release。

在**图形会话**中可另做学生端 GUI / 答题卡烟测：

```bash
cargo build --locked -p client
bash scripts/smoke-answers.sh
```

脚本会短暂打开 debug 学生端，填入合成答案并用 release builder 校验加密答题卡。调试开关在 release 构建中无效，**不要分发** `target/smoke-answers/` 下的调试包。布局以程序自报的 rect、`contains`、`center_offset` 为准，不用屏幕截图判断。
