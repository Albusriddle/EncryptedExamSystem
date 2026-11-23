# Arch Linux: native build and usage

The teacher GUI (`ExamAdmin`), student GUI (`exam-client`) and optional LAN web
server (`exam-server`) run natively on Linux. Wine is not required. Build on the
operating system your students use: a Linux ELF student package does **not** run
on Windows, and a Windows `.exe` does **not** run natively on Linux. Packaging does
not cross-compile the student template.

## 1. Install dependencies

Use a full Arch upgrade, not a partial upgrade:

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

`rust-toolchain.toml` selects stable for this repository; you do not need to change
your global default toolchain. `curl` and CA certificates are required **at exam
unlock time**, not only at build time. Arch's `curl` package brings the system CA
certificate dependencies. Keep the trust store up to date.

The scripts use `uv` with Python 3.13, downloading that interpreter if necessary.
This avoids rolling-release Python upgrades arriving before PySide6/PyInstaller
support. Dependencies live in `admin/.venv`; do **not** use `sudo pip` or
`--break-system-packages`. Initial setup needs Internet access.

## 2. Build and launch from source

From the repository root:

```bash
bash scripts/build-linux.sh
bash scripts/run-admin.sh
```

The build creates native executables under `target/release/`, prepares the Python
GUI environment and runs the admin dependency/crypto self-test. The launcher works
from any working directory and forwards command-line arguments:

```bash
bash scripts/run-admin.sh --version
bash scripts/run-admin.sh --selftest
```

Equivalent manual commands:

```bash
cargo build --locked --release --workspace
cd admin
uv sync --locked --python 3.13 --extra gui --extra build --group dev
uv run --locked python -m app
```

New projects automatically load the repository's math, English and question-type
showcase banks from `banks/demo/`, including in the standalone bundle. These are
**demo content only**, not production exam banks. Remove them before importing
your own banks for a real exam. Opening a saved project keeps its banks unchanged.

In the GUI, configure students/sessions, generate a master key and build the
student package. On Linux the default output is `dist/StudentClient`
(without `.exe`). The template is `target/release/exam-client`; the builder sets
the output's executable permission. Keep the master key private using the GUI's
explicit copy/export workflow; never distribute it with student packages.

Students run the generated package, **not** the bare template:

```bash
chmod +x ./StudentClient  # may be necessary after downloading or copying
./StudentClient
```

The initial unlock still requires at least two agreeing HTTPS time sources.
After unlocking, answering is offline. No Linux-specific bypass of the time gate
or encryption is enabled. Existing accepted security limitations remain documented
in `安全边界与威胁模型.md` and `风险接受记录.md`.

## 3. 推荐：AppImage 桌面版

```bash
bash scripts/build-linux.sh --appimage
```

This produces:

- `dist/ExamAdmin-<version>-x86_64.AppImage`: the complete teacher application.
- `dist/stub-template-x86_64.AppImage`: the student **template**, not an exam yet.
- The traditional Linux ZIP, also containing the AppImage student template.

### Teacher and student workflow

1. Keep the teacher AppImage in a permanent directory. In the file manager's
   **Properties → Permissions**, allow execution, then double-click it. The
   equivalent terminal command is `chmod +x ExamAdmin-*.AppImage` followed by
   `./ExamAdmin-<version>-x86_64.AppImage`.
2. Optionally choose **帮助 → 创建桌面启动器…** to add an application-menu entry;
   pin it using your desktop environment. Installation is per-user, requires no
   `sudo`, and happens only on request. Move the AppImage later? Run that action
   again to update its absolute path.
3. Use the same five tabs as Windows: **题库 → 学生 → 场次 → 打包 → 收卷判分**.
   New projects still contain 3 public demo banks / 14 questions, and no students
   or sessions; opening a saved project does not add demo banks.
4. The packaging page defaults to `stub-template.AppImage` and an output named
   `StudentClient.AppImage`. Generate the master and build as usual. No AppImage
   build tools are needed for each exam: the existing Rust builder appends the
   same encrypted exam format to the prebuilt template.
5. Give students **only the generated student AppImage**, never the teacher app,
   master, projects or PIN table. Students allow execution, double-click, and
   enter their student ID / optional PIN. The initial network-time check is
   unchanged. Collect the encrypted `.eea` files after the exam.

The teacher AppImage includes Python, Qt, builder, server, student template and
Noto CJK regular/bold fonts. The student AppImage includes its native GUI and
fonts. Neither requires Python or Wine to be installed. Qt uses a consistent
light Fusion style on Linux while preserving the Windows native style.

AppImages are read-only mounted applications. The client reads the exam from the
**outer AppImage**, not its inner ELF. Answers are saved beside the outer student
file if writable; teacher projects similarly live beside the outer teacher file,
with the existing user-directory fallback. Nothing persistent is written inside
`/tmp/.mount_*` or an extract-and-run directory.

### Runtime requirements and portability

A graphical Linux x86_64 session, working OpenGL/Wayland or X11 drivers, `curl`
and system CA certificates are still required. AppImage does not emulate Windows
or make an Arch-linked glibc binary work on arbitrarily old distributions.
The local Arch build is verified on Arch. CI's separate `linux-appimage` job
builds on Ubuntu 22.04 (glibc 2.35) as an older distribution baseline; its artifact
is `exam-admin-linux-appimage`. Test on the intended distribution before an exam;
a baseline build alone is not a claim that every Linux desktop was tested.

The bundled AppImage runtime is pinned to upstream release `20251108` and its
SHA-256 is checked before use. It statically includes FUSE support, so it does
**not** need `libfuse.so.2`. It normally needs access to `/dev/fuse`. In restricted
containers or desktops where mounting is unavailable, use:

```bash
./ExamAdmin-<version>-x86_64.AppImage --appimage-extract-and-run
./StudentClient.AppImage --appimage-extract-and-run
```

Normal desktop launchers use the FUSE route. The extraction option is also used
by CI; it must preserve the same payload and persistent-file behavior. Licenses
are included inside the image under `usr/share/doc/EncryptedExamSystem/` and in
the teacher's directory bundle. To inspect/replace Qt or fonts, use
`--appimage-extract`, modify `squashfs-root`, and rebuild with the provided
AppImage packaging script; the original source and license rights remain intact.

### Windows comparison

The Windows `StudentClient.exe` was launched with Bottles/Soda. Its reported X11
client area was **760×720**, matching the native student viewport. Wine reported
missing Chinese system fonts; the Linux AppImages now carry Noto CJK instead of
relying on each user's font setup. The downloaded official Windows teacher ZIP
was checksum-verified, but its Qt runtime required `icuuc.dll`, unavailable in
that Wine environment. This is a Wine compatibility limitation, not evidence of
native Windows failure; Windows CI is retained. No real student PIN or exam
answers were used for comparison.

The Linux teacher AppImage's diagnostics reported **1080×760**, all five tabs,
a visible watermark, the AppImage template/output defaults, and the demo banks.
A debug student AppImage reported a fully contained welcome dialog at
`center_offset=(0.0,-0.5)` in a 760×720 viewport, loading its Chinese font from
inside the AppImage. These checks use program/window diagnostics, not screenshots.

## 3.1 Traditional standalone admin bundle

```bash
bash scripts/build-linux.sh --package
```

Outputs:

- `dist/ExamAdmin/ExamAdmin`: PyInstaller onedir Linux application.
- `dist/ExamAdmin/stub-template`: native student template.
- `dist/ExamAdmin/_internal/`: bundled Python, Qt, builder and server dependencies.
- `dist/ExamAdmin-<version>-linux.zip`: teacher bundle, including docs, licenses,
  demo banks and native tools.

Extract the ZIP and run `./ExamAdmin` **inside its `ExamAdmin/` subdirectory**.
Keep the complete directory, especially `_internal/`. Build and distribute Linux
bundles separately from Windows bundles. The Linux build depends on system
libraries and glibc; it is not a static binary or a promise of compatibility with
older Linux distributions. The font and graphics runtime packages listed above
are also needed for the traditional bare ELF student package. AppImage bundles
its Chinese fonts; the graphics, curl and CA requirements still apply.

## 4. Wayland, X11 and fonts

The Rust student GUI supports both Wayland and X11. Qt normally selects the
appropriate backend. If a Qt Wayland plugin/driver is unavailable, try XWayland:

```bash
sudo pacman -S --needed xorg-xwayland
QT_QPA_PLATFORM=xcb bash scripts/run-admin.sh
```

To select Qt's native Wayland backend explicitly:

```bash
QT_QPA_PLATFORM=wayland bash scripts/run-admin.sh
```

Run a graphical session, not a plain TTY or headless SSH shell. Do not set
`QT_QPA_PLATFORM=offscreen` for an actual exam; that is for tests only.

Chinese text needs a CJK font. The student client recognizes Arch's
`/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc` and its bold counterpart,
provided by `noto-fonts-cjk`. If text appears as boxes, install that package and
restart the application. For Qt plugin failures, `QT_DEBUG_PLUGINS=1` helps identify
missing shared libraries; `ldd` can identify `not found` dependencies.

## 5. User files

| File | Linux location |
|---|---|
| Teacher settings, receipt key, default source-mode projects | `${XDG_CONFIG_HOME:-$HOME/.config}/ExamSystem/` |
| Teacher crash log | `${XDG_STATE_HOME:-$HOME/.local/state}/ExamSystem/admin-crash.log` |
| Student welcome-dismissal marker | `${XDG_DATA_HOME:-$HOME/.local/share}/ExamSystem/skip_welcome` |
| Student answers | Beside `StudentClient` / the outer student AppImage if writable; otherwise `${XDG_DATA_HOME:-$HOME/.local/share}/ExamSystem/answers/` |

Only absolute XDG overrides are accepted. Explicit `LOCALAPPDATA`/`APPDATA`
overrides in the teacher app remain supported for existing portable deployments;
unset those variables for normal XDG behavior. A packaged teacher app still tries
`projects/` beside its executable first (beside the outer file for AppImage),
then falls back to the user directory.
Older source builds wrote settings to `~/ExamSystem/settings.ini` and the student
welcome marker to `~/ExamSystem/skip_welcome`; copy them to the new locations if
you want to keep those preferences. Collect `.eea` answer files wherever they were
saved; existing answer formats are unchanged.

Store private teacher projects and keys on a Linux filesystem with working file
permissions. FAT/exFAT and some NTFS mounts cannot enforce Unix `0600` permissions.
If a binary reports `Permission denied` despite `chmod +x`, check for a `noexec`
mount and move it to an executable Linux filesystem (for example, your home
folder). Keep the student package and generated `.eea` files together when
collecting submissions.

## 6. Validation

```bash
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
(cd admin && QT_QPA_PLATFORM=offscreen uv run --locked pytest -q)
(cd admin && uv run --locked ruff check . && uv run --locked ruff format --check .)
bash scripts/verify-embed.sh
bash scripts/preflight-release.sh
bash scripts/smoke-web.sh
# After building AppImages: real time gate, self-reading payload and rejection checks
admin/.venv/bin/python scripts/smoke-appimage.py --native
```

The end-to-end scripts contact the real HTTPS time sources. On a restricted
network they must fail closed rather than silently skip time verification. The
Arch container CI job also builds the Linux teacher bundle and runs its self-test;
the existing Windows job remains in place for Windows-specific regressions.
Both Arch and Ubuntu-baseline CI jobs exercise AppImage packaging and extraction;
no release tag is created by building or uploading CI artifacts.

For a real student GUI/answer-sheet smoke test **inside a desktop session**:

```bash
cargo build --locked -p client
bash scripts/smoke-answers.sh
```

This briefly opens a debug student GUI, fills synthetic answers and checks the
encrypted answer sheet with the release builder. The automation hooks are absent
from release builds; never distribute the debug package under `target/smoke-answers/`.
