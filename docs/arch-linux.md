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
  base-devel git rustup uv python curl pkgconf zip unzip \
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

In the GUI, import banks, configure students/sessions, generate a master key and
build the student package. On Linux the default output is `dist/StudentClient`
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

## 3. Optional standalone admin bundle

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
are also needed on student machines.

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
| Student answers | Beside `StudentClient` if writable; otherwise `${XDG_DATA_HOME:-$HOME/.local/share}/ExamSystem/answers/` |

Only absolute XDG overrides are accepted. Explicit `LOCALAPPDATA`/`APPDATA`
overrides in the teacher app remain supported for existing portable deployments;
unset those variables for normal XDG behavior. A packaged teacher app still tries
`projects/` beside its executable first, then falls back to the user directory.
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
```

The end-to-end scripts contact the real HTTPS time sources. On a restricted
network they must fail closed rather than silently skip time verification. The
Arch container CI job also builds the Linux teacher bundle and runs its self-test;
the existing Windows job remains in place for Windows-specific regressions.

For a real student GUI/answer-sheet smoke test **inside a desktop session**:

```bash
cargo build --locked -p client
bash scripts/smoke-answers.sh
```

This briefly opens a debug student GUI, fills synthetic answers and checks the
encrypted answer sheet with the release builder. The automation hooks are absent
from release builds; never distribute the debug package under `target/smoke-answers/`.
