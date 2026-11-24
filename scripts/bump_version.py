#!/usr/bin/env python3
"""发版时统一改版本号：**只改标记位**，不碰正文里的历史版本。

## 为什么需要它

以前发版是「把 vX.Y.Z 全库替换」。这会顺手改掉文档里的**历史叙述**：

- `v1.4.5 起，每次保存都加密落盘` —— 说的是这个能力从哪个版本开始有；
- `## 更新摘要（v1.4.6）` —— 历史发版说明；
- `第一百一十二轮（2026-11-24，v1.4.7）` —— 审阅轮次记录。

v1.4.7 发版时就这样误改了 3 处，靠人工回滚才发现。这个脚本把「该改哪里」写死成
标记位，并提供 `--check` 给发布预检 / CI 用：只要有一处标记不是当前 `VERSION`，
它就退出 1 并列出具体文件与期望值。

## 用法

```bash
python scripts/bump_version.py 1.4.8             # 改到 1.4.8（先校验，再写盘）
python scripts/bump_version.py 1.4.8 --dry-run   # 只打印将要改的文件
python scripts/bump_version.py --check           # 只检查标记是否一致
```

**发版顺序**：先在 `docs/release-notes.md` 里手写新版本的
`## 更新摘要（vX.Y.Z）` 小节，再跑本脚本——脚本只会更新「当前版本」与版本计数，
不会替你编发版内容。

## 会改的标记位

- `VERSION`、`Cargo.toml` 的 `[workspace.package]`、`admin/pyproject.toml`、
  `admin/uv.lock`、`Cargo.lock` 里四个 workspace crate 的版本；
- 各文档的「适用版本」标记（`适用版本：**vX.Y.Z**` / `适用版本 **vX.Y.Z**；`）；
- `README.md` 的「当前版本：**X.Y.Z**」与「## 8. 测试与验证基线（vX.Y.Z）」；
- 架构文档里的「# vX.Y.Z：N 个 #[test]」「# vX.Y.Z：N 用例」（只改版本，不动计数）；
- 代码审阅报告的「## 当前基线（vX.Y.Z）」；
- `docs/release-notes.md` 的「> 当前版本 **X.Y.Z**」与「共 N 个版本」（计数自动算）。

## 明确不动

`## 更新摘要（vX.Y.Z）` 各节、`vX.Y.Z 起` / `vX.Y.Z 之前`、审阅报告的轮次标题。
"""

from __future__ import annotations

import argparse
import os
import re
import sys
from pathlib import Path

#: 需要同步版本的 workspace crate（Cargo.lock 里的 package 名）。
WORKSPACE_CRATES = ("builder", "client", "common", "server")

#: 架构文档里两条基线数字的写法（版本跟着发版走，计数不归本脚本管）。
ARCH_TEST_LINE = re.compile(r"(# v)(\d+\.\d+\.\d+)(：\d+ 个 #\[test\])")
ARCH_CASES_LINE = re.compile(r"(# v)(\d+\.\d+\.\d+)(：\d+ 用例)")

VERSION_RE = r"\d+\.\d+\.\d+"

#: 必须带「适用版本」标记的文档（各自恰好一处）。新增带标记的文档时加进这里，
#: 这样「标记被删掉」也会被发现，而不是静默跳过。
DOC_VERSION_FILES = (
    "docs/出题质检与难度口径.md",
    "docs/图片使用教程.md",
    "docs/时间令牌协议.md",
    "docs/题库JSON规范.md",
    "docs/安全边界与威胁模型.md",
    "docs/教师使用手册.md",
)


class MarkerDoc:
    """一个待改文件：读进来、按规则替换、最后统一写盘。

    所有问题先收集起来；有任何问题就**一个文件都不写**，避免半成品状态。
    """

    def __init__(self, root: Path, rel: str, problems: list[str]) -> None:
        self.rel = rel
        self.path = root / rel
        self.problems = problems
        self.original = ""
        self.text = ""
        if not self.path.is_file():
            problems.append(f"缺少文件：{rel}")
            return
        self.original = self.path.read_text(encoding="utf-8")
        self.text = self.original

    @property
    def missing(self) -> bool:
        return not self.path.is_file()

    def sub(self, pattern: str, version: str, expected: int, label: str) -> None:
        if self.missing:
            return
        new, n = re.subn(pattern, lambda m: m.group(1) + version + m.group(3), self.text)
        if n != expected:
            self.problems.append(f"{self.rel}：{label} 期望 {expected} 处，实际 {n} 处")
        self.text = new

    def section_version(self, header: str, version: str, label: str) -> None:
        """把 `header` 之后的第一处 `version = "..."` 改成新版本。"""
        if self.missing:
            return
        idx = self.text.find(header)
        if idx < 0:
            self.problems.append(f"{self.rel}：找不到 {header}")
            return
        pattern = re.compile(r'(version = ")([^"]+)(")')
        match = pattern.search(self.text, idx)
        if not match:
            self.problems.append(f"{self.rel}：{label} 里找不到 version 字段")
            return
        self.text = self.text[: match.start(2)] + version + self.text[match.end(2) :]

    def changed(self) -> bool:
        """内容是否与磁盘上的不同（`bump` 与 `--check` 共用同一判定）。"""
        return not self.missing and self.text != self.original


def _release_note_sections(text: str) -> list[str]:
    return re.findall(r"^## 更新摘要（v(" + VERSION_RE + r")）", text, flags=re.M)


def plan(root: Path, version: str) -> tuple[list[MarkerDoc], list[str]]:
    """计算所有改动；返回（文档列表, 问题列表）。"""
    problems: list[str] = []
    docs: dict[str, MarkerDoc] = {}

    def doc(rel: str) -> MarkerDoc:
        if rel not in docs:
            docs[rel] = MarkerDoc(root, rel, problems)
        return docs[rel]

    # 1) VERSION 文件：整份内容就是版本号
    version_doc = doc("VERSION")
    if not version_doc.missing:
        version_doc.text = f"{version}\n"

    # 2) 三处版本来源 + 两个锁文件
    doc("Cargo.toml").section_version("[workspace.package]", version, "[workspace.package]")
    doc("admin/pyproject.toml").section_version("[project]", version, "[project]")
    doc("admin/uv.lock").sub(
        r'(name = "exam-admin"\nversion = ")([^"]+)(")', version, 1, "exam-admin 版本"
    )
    lock = doc("Cargo.lock")
    for crate in WORKSPACE_CRATES:
        lock.sub(
            rf'(name = "{crate}"\nversion = ")([^"]+)(")',
            version,
            1,
            f"{crate} 版本",
        )

    # 3) README：当前版本 + 基线小节标题
    readme = doc("README.md")
    readme.sub(r"(当前版本：\*\*)(\d+\.\d+\.\d+)(\*\*)", version, 1, "当前版本")
    readme.sub(r"(## 8\. 测试与验证基线（v)(\d+\.\d+\.\d+)(）)", version, 1, "基线小节标题")

    # 4) 架构文档：适用版本 + 两条基线数字
    arch = doc("docs/保密考试系统_架构与实施方案.md")
    arch.sub(r"(> 适用版本：\*\*v)(\d+\.\d+\.\d+)(\*\*)", version, 1, "适用版本")
    arch.sub(ARCH_TEST_LINE.pattern, version, 1, "Rust 测试基线")
    arch.sub(ARCH_CASES_LINE.pattern, version, 1, "Python 用例基线")

    # 5) 审阅报告：当前基线
    review = doc("docs/代码审阅报告.md")
    review.sub(r"(## 当前基线（v)(\d+\.\d+\.\d+)(）)", version, 1, "当前基线")

    # 6) release-notes：当前版本 + 版本计数（计数按小节数自动算）
    notes = doc("docs/release-notes.md")
    if not notes.missing:
        notes.sub(r"(> 当前版本 \*\*)(\d+\.\d+\.\d+)(\*\*)", version, 1, "当前版本")
        sections = _release_note_sections(notes.text)
        if version not in sections:
            problems.append(
                f"docs/release-notes.md：缺少「## 更新摘要（v{version}）」小节——"
                "先手写这一节，再跑本脚本"
            )
        if len(sections) != len(set(sections)):
            problems.append("docs/release-notes.md：有重复的版本小节")
        count = len(set(sections))
        notes.text, matched = re.subn(r"(共 )\d+( 个版本)", rf"\g<1>{count}\g<2>", notes.text)
        if matched != 1:
            problems.append(f"docs/release-notes.md：「共 N 个版本」 期望 1 处，实际 {matched} 处")

    # 7) 其余带「适用版本」标记的文档（历史叙述里的「vX.Y.Z 起」不在匹配范围内）
    for rel in DOC_VERSION_FILES:
        d = doc(rel)
        if d.missing:
            continue
        matches = 0
        # 两种写法都要覆盖：`适用版本：**vX.Y.Z**` 与 `适用版本 **vX.Y.Z**；`
        for pattern in (
            r"(适用版本：\*\*v)\d+\.\d+\.\d+(\*\*)",
            r"(适用版本 \*\*v)\d+\.\d+\.\d+(\*\*)",
        ):
            d.text, n = re.subn(pattern, rf"\g<1>{version}\g<2>", d.text)
            matches += n
        if matches != 1:
            problems.append(f"{rel}：「适用版本」标记期望 1 处，实际 {matches} 处")

    return list(docs.values()), problems


def _first_diff(original: str, current: str) -> str:
    """给出第一处差异，便于直接看出「哪个标记没跟上」。"""
    old_lines = original.splitlines()
    new_lines = current.splitlines()
    for index, (old, new) in enumerate(zip(old_lines, new_lines, strict=False), 1):
        if old != new:
            return f"第 {index} 行\n      现在：{old}\n      应为：{new}"
    return f"行数不同：{len(old_lines)} → {len(new_lines)}"


def write_all(changed: list[MarkerDoc]) -> int:
    """两阶段写盘：先把所有新内容写到 `<文件>.tmp-<pid>`，全部成功后再统一 `os.replace`。

    为什么要两阶段：逐个 `write_text` 时，中途失败（磁盘满、文件被占用）会留下
    「版本号一半新一半旧」的仓库，而 `--check` 只能事后发现。这里保证「要么全改、
    要么全不改」；万一 `os.replace` 阶段仍失败，用内存里的原文把已替换的恢复回去，
    再删掉剩余 tmp。
    """
    staged: list[tuple[Path, MarkerDoc]] = []
    replaced: list[MarkerDoc] = []
    try:
        for doc in changed:
            tmp = doc.path.with_name(f"{doc.path.name}.tmp-{os.getpid()}")
            tmp.write_text(doc.text, encoding="utf-8", newline="")
            staged.append((tmp, doc))
        for tmp, doc in staged:
            os.replace(tmp, doc.path)
            replaced.append(doc)
    except OSError as exc:
        for doc in replaced:
            doc.path.write_text(doc.original, encoding="utf-8", newline="")
        for tmp, _doc in staged:
            tmp.unlink(missing_ok=True)
        print(f"写盘失败，已回滚：{exc}", file=sys.stderr)
        return 1
    return 0


def check(root: Path, version: str) -> int:
    docs, problems = plan(root, version)
    if problems:
        _report(problems)
        return 1
    stale = [d for d in docs if not d.missing and d.text != d.original]
    if stale:
        _report([f"{d.rel}：{_first_diff(d.original, d.text)}" for d in stale])
        print(f"\n修法：python scripts/bump_version.py {version}")
        return 1
    print(f"版本标记一致：{version}（{len(docs)} 个文件）")
    return 0


def bump(root: Path, version: str, dry_run: bool) -> int:
    docs, problems = plan(root, version)
    if problems:
        _report(problems)
        return 1
    changed = [d for d in docs if d.changed()]
    if not changed:
        print(f"已经是 {version}，无需改动")
        return 0
    for d in changed:
        print(f"{'（dry-run）' if dry_run else ''}改：{d.rel}")
    if not dry_run and write_all(changed) != 0:
        return 1
    print(f"\n{'将要' if dry_run else '已'}把 {len(changed)} 个文件改到 {version}")
    if not dry_run:
        print("下一步：跑发布预检（bash scripts/preflight-release.sh）再提交。")
    return 0


def _report(problems: list[str]) -> None:
    print(f"发现 {len(problems)} 个问题，未改动任何文件：", file=sys.stderr)
    for item in problems:
        print(f"  - {item}", file=sys.stderr)


def main() -> int:
    # 输出里有中文；Windows 下 stdout 被管道捕获时默认走 GBK，会把调用方（测试 / CI）
    # 卡在 UnicodeDecodeError 上。显式切到 UTF-8，CI 的 Ubuntu 本来就是 UTF-8。
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8", errors="replace")
        except (AttributeError, ValueError):
            pass

    parser = argparse.ArgumentParser(
        description="发版改版本号：只改标记位，不碰历史叙述",
    )
    parser.add_argument("version", nargs="?", help="目标版本号，例如 1.4.8")
    parser.add_argument("--check", action="store_true", help="只检查标记是否都等于 VERSION")
    parser.add_argument("--dry-run", action="store_true", help="只打印将要改哪些文件")
    parser.add_argument(
        "--root",
        default=str(Path(__file__).resolve().parents[1]),
        help="仓库根目录（测试用）",
    )
    args = parser.parse_args()

    root = Path(args.root)
    if args.check:
        current = (root / "VERSION").read_text(encoding="utf-8").strip()
        return check(root, current)

    if not args.version:
        parser.error("需要目标版本号，或加 --check")
    if not re.fullmatch(VERSION_RE, args.version):
        parser.error(f"版本号格式应为 X.Y.Z，实际 {args.version!r}")
    return bump(root, args.version, args.dry_run)


if __name__ == "__main__":
    sys.exit(main())
