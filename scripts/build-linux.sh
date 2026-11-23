#!/usr/bin/env bash
# 原生 Linux 构建；不使用 Wine，也不向 Arch 的系统 Python 安装 pip 包。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PACKAGE=0
APPIMAGE=0
for arg in "$@"; do
  case "$arg" in
    --package) PACKAGE=1 ;;
    --appimage) PACKAGE=1; APPIMAGE=1 ;;
    -h|--help)
      echo "Usage: bash scripts/build-linux.sh [--package] [--appimage]"
      echo "Build native tools/GUI; --package adds a ZIP, --appimage also adds portable teacher/student AppImages."
      exit 0 ;;
    *) echo "Unknown argument: $arg" >&2; exit 2 ;;
  esac
done
if [ "$(uname -s)" != Linux ]; then
  echo "This script builds Linux binaries. On Windows, use the README build instructions." >&2
  exit 1
fi
for tool in cargo uv curl cc pkg-config; do
  command -v "$tool" >/dev/null || {
    echo "Missing $tool. See docs/arch-linux.md for Arch Linux dependencies." >&2
    exit 1
  }
done
if [ "$PACKAGE" -eq 1 ] && ! command -v zip >/dev/null; then
  echo "Missing zip. Install it with: sudo pacman -S zip" >&2
  exit 1
fi

if [ "$APPIMAGE" -eq 1 ] && ! command -v mksquashfs >/dev/null; then
  echo "Missing mksquashfs. Install it with: sudo pacman -S squashfs-tools" >&2
  exit 1
fi

cd "$ROOT"
cargo build --locked --release --workspace
cd "$ROOT/admin"
# 固定一个受支持的解释器，避免 Arch 滚动升级先于 PySide6/PyInstaller 支持。
# uv 会按需下载它并隔离在 .venv，不修改 /usr/bin/python。
uv sync --locked --python 3.13 --extra gui --extra build --group dev
uv run --locked python -m app --selftest

if [ "$PACKAGE" -eq 1 ]; then
  uv run --locked python buildtools/build_exe.py
  if [ "$APPIMAGE" -eq 1 ]; then
    uv run --locked python -m buildtools.build_appimage
  fi
  # 让发布脚本也使用同一个解释器，而不是发行版中可能不同版本的 python。
  export PATH="$ROOT/admin/.venv/bin:$PATH"
  cd "$ROOT"
  bash scripts/make-release-zip.sh "$(tr -d '[:space:]' < VERSION)"
fi
if [ "$APPIMAGE" -eq 1 ]; then
  printf '\nReady. Double-click dist/ExamAdmin-%s-x86_64.AppImage (allow execution first).\n' "$(tr -d '[:space:]' < "$ROOT/VERSION")"
else
  printf '\nReady. Start the admin GUI with: bash "%s/scripts/run-admin.sh"\n' "$ROOT"
fi
