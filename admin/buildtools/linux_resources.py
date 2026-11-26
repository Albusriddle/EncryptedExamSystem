"""Linux 发布物共享的字体资源；缺字体时构建失败，不能发布方块字界面。"""

from pathlib import Path

FONT_NAMES = ("NotoSansCJK-Regular.ttc", "NotoSansCJK-Bold.ttc")
FONT_DIRS = (Path("/usr/share/fonts/noto-cjk"), Path("/usr/share/fonts/opentype/noto"))


def font_sources() -> list[Path]:
    for directory in FONT_DIRS:
        paths = [directory / name for name in FONT_NAMES]
        if all(path.is_file() for path in paths):
            return paths
    raise FileNotFoundError(
        "缺少 Noto CJK 字体：Arch 安装 noto-fonts-cjk；Debian/Ubuntu 安装 fonts-noto-cjk。"
    )
