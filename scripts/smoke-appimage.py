#!/usr/bin/env python3
"""真实 AppImage 烟测：读取外层 payload、正常解锁、拒绝名单外/未来场次/调试开关。

默认使用无 FUSE 的 extract-and-run，便于 CI；--native 额外验证真正的 FUSE 挂载。
只生成公开合成题目，master 只在内存与子进程环境中出现。
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
from datetime import UTC, datetime, timedelta
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def run(args: list[str], *, env: dict[str, str], ok: bool = True) -> str:
    result = subprocess.run(
        args, env=env, capture_output=True, encoding="utf-8", errors="replace", timeout=180
    )
    text = result.stdout + result.stderr
    master = env.get("EXAM_MASTER")
    if master:
        text = text.replace(master, "[REDACTED]")
    if (result.returncode == 0) != ok:
        raise RuntimeError(f"{Path(args[0]).name} 验证失败（exit={result.returncode}）\n{text}")
    return text


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--native", action="store_true")
    parser.add_argument("--stub", type=Path, default=ROOT / "dist/stub-template-x86_64.AppImage")
    args = parser.parse_args()
    work = ROOT / "target/smoke-appimage/含空格的 student package"
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    banks = work / "banks"
    banks.mkdir()
    (banks / "demo.json").write_text(
        json.dumps(
            {
                "exam_id": "demo",
                "title": "AppImage 合成测试",
                "pick": 1,
                "questions": [
                    {
                        "id": "q1",
                        "type": "single",
                        "stem": "1+1?",
                        "options": ["1", "2"],
                        "answer": "B",
                        "score": 1,
                    }
                ],
            },
            ensure_ascii=False,
        ),
        encoding="utf-8",
    )
    now = datetime.now(UTC)

    def session(sid: int, start: datetime, end: datetime) -> dict:
        return {
            "session_id": sid,
            "start": start.isoformat(),
            "end": end.isoformat(),
            "students": [42],
            "exams": [{"file": "demo.json", "pick": 1}],
        }

    manifest = work / "manifest.json"
    manifest.write_text(
        json.dumps(
            {
                "version": 2,
                "salt": "00112233445566778899aabbccddeeff",
                "kdf": {"m_cost": 8, "t_cost": 1, "p_cost": 1},
                "sessions": [
                    session(1, now - timedelta(hours=1), now + timedelta(hours=1)),
                    session(2, now + timedelta(days=30), now + timedelta(days=30, hours=1)),
                ],
            }
        ),
        encoding="utf-8",
    )
    builder = str(ROOT / "target/release/exam-builder")
    env = os.environ.copy()
    env.pop("EXAM_MASTER", None)
    # 独立捕获，不能把 keygen 输出交给会输出失败详情的日志帮助函数。
    key = subprocess.run(
        [builder, "keygen"], env=env, capture_output=True, encoding="utf-8", check=True
    ).stdout.strip()
    build_env = {**env, "EXAM_MASTER": key}
    image = work / "StudentClient.AppImage"
    run(
        [
            builder,
            "build",
            "--manifest",
            str(manifest),
            "--banks",
            str(banks),
            "--stub",
            str(args.stub.resolve()),
            "--out",
            str(image),
            "--allow-weak-kdf",
        ],
        env=build_env,
    )
    run([builder, "verify", "--package", str(image)], env=build_env)
    print("✓ builder 生成/校验 AppImage（合成题目，未把 master 写盘）")
    cmd = [str(image), "--appimage-extract-and-run"]
    text = run([*cmd, "--student", "42"], env=env)
    assert "解锁成功" in text, text
    print("✓ 无 --package/EXAM_MASTER 参数：从外层 AppImage 解锁")
    denied = run([*cmd, "--student", "999999"], env=env, ok=False)
    assert "不在场次" in denied and "名单中" in denied, denied
    print("✓ 名单外学生拒绝")
    future = run([*cmd, "--student", "42", "--session", "2"], env=env, ok=False)
    assert "不在当前时间窗内" in future, future
    print("✓ 未来场次拒绝（没有绕过真实网络时间门禁）")
    debug = run([*cmd, "--student", "42", "--skip-timegate"], env=env, ok=False)
    assert "unexpected argument" in debug, debug
    print("✓ release 不接受 --skip-timegate")
    if args.native:
        native = run([str(image), "--student", "42"], env=env)
        assert "解锁成功" in native, native
        print("✓ FUSE 原生挂载运行成功")
    print("✓ AppImage 烟测全部通过")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
