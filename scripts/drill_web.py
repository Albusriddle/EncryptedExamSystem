#!/usr/bin/env python3
"""网页考试「局域网演练」：模拟 30–50 人同时登录、作答、进程崩溃、交卷、校验。

它回答的是现场最关心的三个问题：

1. **并发会不会卡**：N 个学生同时登录 / 同时交卷时，请求的 p50 / p95 / 最慢是多少？
2. **教师机崩溃会不会丢答案**：交卷前 `kill -9` 掉服务、再重启，学生重登能否拿回作答？
3. **收上来的卡对不对**：N 份 `.eea` 是否齐全、能否被 `exam-builder verify-answer-sheet` 验过、
   事件日志与状态文件数量是否一致？

为什么需要它：真实考场里 30–50 人同时点「交卷」是最容易暴露并发问题的时刻，而
`smoke-web.sh` 只跑单个学生。这个脚本把那次演练变成可重复的一条命令，并能进 CI
（CI 用 `--students 12` 压缩规模）。

用法（通常经 `bash scripts/drill-web.sh` 调用，它会补上二进制路径）：

    python scripts/drill_web.py --students 40

退出码：0 全部通过；1 有断言失败；2 参数/环境问题。
"""

from __future__ import annotations

import argparse
import http.client
import json
import os
import shutil
import socket
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]

#: 每个学生演练时作答的题数（题量不足时按实际题量）。
ANSWERS_PER_STUDENT = 3


# ---------------------------------------------------------------------------
# 基础设施
# ---------------------------------------------------------------------------


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_port(port: int, deadline_s: float = 30.0) -> bool:
    """等服务端真的开始监听（启动时先校时，绑定端口发生在之后）。"""
    end = time.monotonic() + deadline_s
    while time.monotonic() < end:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.settimeout(0.5)
            if s.connect_ex(("127.0.0.1", port)) == 0:
                return True
        time.sleep(0.1)
    return False


class Server:
    """exam-server 的生命周期。`crash()` 用 SIGKILL 模拟断电/被误杀。"""

    def __init__(
        self, exe: Path, package: Path, keyfile: Path, port: int, answers: Path, log: Path
    ):
        self.exe = exe
        self.package = package
        self.keyfile = keyfile
        self.port = port
        self.answers = answers
        self.log = log
        self.proc: subprocess.Popen | None = None

    def start(self) -> None:
        self.answers.mkdir(parents=True, exist_ok=True)
        with open(self.log, "ab") as log:
            self.proc = subprocess.Popen(
                [
                    str(self.exe),
                    "--package",
                    str(self.package),
                    "--keyfile",
                    str(self.keyfile),
                    "--port",
                    str(self.port),
                    "--answers-dir",
                    str(self.answers),
                    # 演练关心并发与恢复，不关心外网取时；固定 off 让结果可复现。
                    "--time-check",
                    "off",
                    # 演练里所有「学生」都来自同一台机器（同一个源 IP），会被按 IP 的登录
                    # 限速当成暴力破解。真实考场每人一个 IP，所以这里关掉——限速本身
                    # 由单测（第 21 次 429 / 换 IP 放行）覆盖。
                    "--login-rate-limit",
                    "0",
                    # v1.5.0 起服务端默认 HTTPS；演练测的是并发与崩溃恢复，
                    # 不掺 TLS 握手开销，也免得脚本里到处 -k。TLS 本身由
                    # scripts/smoke-web.sh（走 https）与 Rust 的 TLS 端到端用例覆盖。
                    "--tls",
                    "off",
                ],
                stdout=log,
                stderr=subprocess.STDOUT,
            )
        if not wait_port(self.port):
            self.tail_log()
            raise RuntimeError(f"服务端 {self.port} 端口未就绪")

    def crash(self) -> None:
        """模拟教师机崩溃 / 断电：直接杀进程，不给它收尾的机会。"""
        if self.proc is not None:
            self.proc.kill()
            self.proc.wait(timeout=10)
            self.proc = None

    def stop(self) -> None:
        if self.proc is not None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
            self.proc = None

    def tail_log(self, lines: int = 20) -> None:
        if self.log.is_file():
            text = self.log.read_text(encoding="utf-8", errors="replace")
            print("--- server.log 尾部 ---", file=sys.stderr)
            print("\n".join(text.splitlines()[-lines:]), file=sys.stderr)


def http_json(
    port: int,
    method: str,
    path: str,
    body: dict | None = None,
    token: str | None = None,
    csrf: bool = False,
    timeout: float = 60.0,
) -> tuple[int, dict, str, float]:
    """发一个请求，返回 (状态码, JSON, Set-Cookie, 耗时秒)。"""
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=timeout)
    headers: dict[str, str] = {}
    data = None
    if body is not None:
        data = json.dumps(body).encode("utf-8")
        headers["Content-Type"] = "application/json"
    if csrf:
        headers["X-EES-CSRF"] = "1"
    if token:
        headers["Cookie"] = f"ees_token={token}"
    started = time.perf_counter()
    try:
        conn.request(method, path, body=data, headers=headers)
        resp = conn.getresponse()
        raw = resp.read()
        elapsed = time.perf_counter() - started
        cookie = resp.getheader("Set-Cookie") or ""
        status = resp.status
    finally:
        conn.close()
    try:
        payload = json.loads(raw) if raw else {}
    except json.JSONDecodeError:
        payload = {"_raw": raw[:200].decode("utf-8", "replace")}
    return status, payload, cookie, elapsed


def token_from_cookie(cookie: str) -> str:
    for part in cookie.split(";"):
        part = part.strip()
        if part.startswith("ees_token="):
            return part[len("ees_token=") :]
    return ""


def percentile(values: list[float], pct: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    idx = min(len(ordered) - 1, max(0, int(round(pct / 100.0 * (len(ordered) - 1)))))
    return ordered[idx]


def summarize(label: str, values: list[float]) -> str:
    if not values:
        return f"{label}: （无样本）"
    return (
        f"{label}: n={len(values)} "
        f"p50={percentile(values, 50) * 1000:.0f}ms "
        f"p95={percentile(values, 95) * 1000:.0f}ms "
        f"max={max(values) * 1000:.0f}ms"
    )


# ---------------------------------------------------------------------------
# 演练步骤
# ---------------------------------------------------------------------------


def build_package(builder: Path, work: Path, students: list[int]) -> tuple[Path, Path, str]:
    master = subprocess.run(
        [str(builder), "keygen"],
        capture_output=True,
        encoding="utf-8",
        timeout=60,
        check=True,
    ).stdout.strip()

    now = time.time()
    start = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(now - 1800))
    end = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(now + 7200))
    manifest = {
        "version": 2,
        "salt": "00112233445566778899aabbccddeeff",
        "sessions": [
            {
                "session_id": 1,
                "start": start,
                "end": end,
                "students": students,
                # demo 题库只有 6 题；演练每人答 3 题，抽 3 题即可（抽题数超过题量会直接报错）。
                "exams": [{"file": "demo/math.json", "pick": ANSWERS_PER_STUDENT}],
            }
        ],
    }
    manifest_path = work / "live.json"
    manifest_path.write_text(json.dumps(manifest, ensure_ascii=False), encoding="utf-8")

    package = work / "pkg"
    keyfile = work / "key.txt"
    # 主密钥只走环境变量：命令行参数会出现在进程列表里（AGENTS §5），
    # builder 的 build/verify/verify-answer-sheet 都支持 EXAM_MASTER 回退。
    env = dict(os.environ, EXAM_MASTER=master)
    proc = subprocess.run(
        [
            str(builder),
            "build",
            "--manifest",
            str(manifest_path),
            "--banks",
            str(REPO_ROOT / "banks"),
            "--stub",
            str(REPO_ROOT / "target" / "release" / exe_name("exam-client")),
            "--out",
            str(package),
            "--emit-keyfile",
            str(keyfile),
        ],
        capture_output=True,
        encoding="utf-8",
        timeout=300,
        env=env,
    )
    if proc.returncode != 0 or not package.is_file():
        print("打包失败：", file=sys.stderr)
        print((proc.stdout or "")[-2000:], file=sys.stderr)
        print((proc.stderr or "")[-2000:], file=sys.stderr)
        raise RuntimeError("exam-builder build 失败")
    return package, keyfile, master


def exe_name(stem: str) -> str:
    return stem + (".exe" if os.name == "nt" else "")


def login_and_answer(port: int, student: int) -> dict:
    """一个学生的完整「登录 → 取卷 → 作答」流程，返回测量数据与错误。"""
    result: dict = {"student": student, "errors": [], "latencies": {}}
    try:
        status, payload, cookie, dt = http_json(
            port,
            "POST",
            "/api/login",
            {"student_id": student, "pin": None},
            # 服务端登录也校验 CSRF 头（防登录 CSRF），演练必须与浏览器一致。
            csrf=True,
        )
        result["latencies"]["login"] = dt
        if status != 200:
            result["errors"].append(f"登录 {status}: {payload}")
            return result
        token = token_from_cookie(cookie)
        if not token:
            result["errors"].append("登录响应没有 ees_token cookie")
            return result

        status, paper, _, dt = http_json(port, "GET", "/api/paper", token=token)
        result["latencies"]["paper"] = dt
        if status != 200:
            result["errors"].append(f"取卷 {status}: {paper}")
            return result
        items = [item for section in paper["paper"]["sections"] for item in section["items"]]
        if not items:
            result["errors"].append("试卷没有题目")
            return result
        result["qids"] = [item["id"] for item in items]

        save_times: list[float] = []
        for item in items[:ANSWERS_PER_STUDENT]:
            status, body, _, dt = http_json(
                port,
                "PUT",
                "/api/answer",
                {"qid": item["id"], "value": "B" if item.get("options") else "演练答案"},
                token=token,
                csrf=True,
            )
            save_times.append(dt)
            if status != 200:
                result["errors"].append(f"保存 {status}: {body}")
        result["latencies"]["save"] = sum(save_times) / len(save_times) if save_times else 0.0
        result["token"] = token
        result["answers"] = {
            item["id"]: ("B" if item.get("options") else "演练答案")
            for item in items[:ANSWERS_PER_STUDENT]
        }
    except Exception as exc:  # noqa: BLE001 - 演练需要把任何异常汇总成可读失败
        result["errors"].append(f"异常：{exc!r}")
    return result


def run_phase(port: int, students: list[int], workers: int) -> tuple[list[dict], float]:
    started = time.perf_counter()
    with ThreadPoolExecutor(max_workers=workers) as pool:
        results = list(pool.map(lambda sid: login_and_answer(port, sid), students))
    return results, time.perf_counter() - started


def recheck_after_restart(port: int, previous: list[dict]) -> tuple[list[str], dict[int, str]]:
    """重启后逐个重登，核对未交卷的作答是否还在；返回 (失败列表, 新 token 表)。

    重启会清掉内存里的会话 token，所以后面交卷必须用这里换来的新 token。
    """
    failures: list[str] = []
    tokens: dict[int, str] = {}
    for item in previous:
        if item["errors"]:
            continue
        sid = item["student"]
        status, payload, cookie, _ = http_json(
            port,
            "POST",
            "/api/login",
            {"student_id": sid, "pin": None},
            csrf=True,
        )
        if status != 200:
            failures.append(f"{sid}: 重启后登录失败 {status} {payload}")
            continue
        token = token_from_cookie(cookie)
        tokens[sid] = token
        status, paper, _, _ = http_json(port, "GET", "/api/paper", token=token)
        if status != 200:
            failures.append(f"{sid}: 重启后取卷失败 {status} {paper}")
            continue
        if paper.get("submitted"):
            failures.append(f"{sid}: 重启后不应是已交卷")
            continue
        got = paper.get("answers") or {}
        for qid, want in item.get("answers", {}).items():
            if got.get(qid) != want:
                failures.append(
                    f"{sid}: 重启后答案丢失 {qid}: 期望 {want!r}，实际 {got.get(qid)!r}"
                )
                break
    return failures, tokens


def submit_all(
    port: int, students: list[int], tokens: dict[int, str], workers: int
) -> tuple[list[float], list[str]]:
    def one(sid: int) -> tuple[float, str | None]:
        token = tokens.get(sid)
        if not token:
            return 0.0, f"{sid}: 没有 token，跳过交卷"
        status, payload, _, dt = http_json(port, "POST", "/api/submit", {}, token=token, csrf=True)
        if status != 200:
            return dt, f"{sid}: 交卷 {status} {payload}"
        return dt, None

    started = time.perf_counter()
    with ThreadPoolExecutor(max_workers=workers) as pool:
        outcomes = list(pool.map(one, students))
    wall = time.perf_counter() - started
    latencies = [dt for dt, _ in outcomes]
    errors = [err for _, err in outcomes if err]
    latencies.append(wall / max(1, len(students)))  # 仅供报告，不参与断言
    return latencies, errors


def verify_sheets(builder: Path, package: Path, master: str, answers: Path) -> list[str]:
    sheets = sorted(answers.glob("*.eea"))
    failures: list[str] = []
    env = dict(os.environ, EXAM_MASTER=master, PYTHONIOENCODING="utf-8")
    for sheet in sheets:
        proc = subprocess.run(
            [
                str(builder),
                "verify-answer-sheet",
                str(sheet),
                "--package",
                str(package),
                "--json",
            ],
            capture_output=True,
            encoding="utf-8",
            timeout=120,
            env=env,
        )
        if proc.returncode != 0:
            failures.append(f"{sheet.name}: verify 退出码 {proc.returncode} {proc.stderr[-300:]}")
    return failures


def count_events(answers: Path) -> tuple[int, int]:
    """返回 (submit 事件条数, 无法解析的行数)。

    第二项是演练的重点之一：并发写事件日志时若发生字节交错，行数看着够、内容却是乱的，
    审计与追责都会失效。所以「能解析」和「条数够」必须一起断言。
    """
    log = answers / "events.jsonl"
    if not log.is_file():
        return 0, 0
    submits = 0
    malformed = 0
    for line in log.read_text(encoding="utf-8", errors="replace").splitlines():
        if not line.strip():
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            malformed += 1
            continue
        if event.get("kind") == "submit":
            submits += 1
    return submits, malformed


def merge_latencies(results: list[dict], key: str) -> list[float]:
    return [item["latencies"][key] for item in results if key in item["latencies"]]


# ---------------------------------------------------------------------------
# 主流程
# ---------------------------------------------------------------------------


def _force_utf8_stdio() -> None:
    """Windows 控制台默认是 GBK，打印 `✓` / 中文说明会直接抛 UnicodeEncodeError。

    演练脚本因为一个对勾崩掉是最没价值的失败方式；这里显式把标准输出改成 UTF-8
    （CI 的 Ubuntu 本来就是 UTF-8，此调用是 no-op）。
    """
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8", errors="replace")
        except (AttributeError, ValueError):
            pass


def main() -> int:
    _force_utf8_stdio()
    parser = argparse.ArgumentParser(description="网页考试局域网演练（并发 + 崩溃恢复 + 收卡校验）")
    parser.add_argument("--students", type=int, default=40, help="参与演练的学生数（默认 40）")
    parser.add_argument(
        "--workers", type=int, default=0, help="并发线程数（默认 = 学生数，上限 64）"
    )
    parser.add_argument("--port", type=int, default=0, help="监听端口（默认自动挑一个空闲端口）")
    parser.add_argument("--work", default="target/web-drill", help="工作目录")
    parser.add_argument("--server", default="", help="exam-server 路径")
    parser.add_argument("--builder", default="", help="exam-builder 路径")
    parser.add_argument("--client", default="", help="exam-client（作为 stub）路径")
    parser.add_argument("--no-restart", action="store_true", help="跳过「崩溃后重启恢复」阶段")
    args = parser.parse_args()

    work = (REPO_ROOT / args.work).resolve()
    binary_dir = REPO_ROOT / "target" / "release"
    server_exe = Path(args.server) if args.server else binary_dir / exe_name("exam-server")
    builder_exe = Path(args.builder) if args.builder else binary_dir / exe_name("exam-builder")

    for exe in (server_exe, builder_exe):
        if not exe.is_file():
            print(
                f"缺少二进制：{exe}\n先跑：cargo build --release -p server -p builder -p client",
                file=sys.stderr,
            )
            return 2

    students = [2021001 + i for i in range(max(1, args.students))]
    workers = args.workers if args.workers > 0 else min(len(students), 64)
    port = args.port or free_port()

    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True, exist_ok=True)
    answers_dir = work / "answers"

    print(f"演练：{len(students)} 名学生，端口 {port}，工作目录 {work}")
    package, keyfile, master = build_package(builder_exe, work, students)
    print(f"考试包：{package}（{package.stat().st_size / 1024:.0f} KB）")

    server = Server(server_exe, package, keyfile, port, answers_dir, work / "server.log")
    failures: list[str] = []
    try:
        # ---- 阶段 1：并发登录 + 取卷 + 作答 ----
        print("\n[1/4] 并发登录 / 取卷 / 作答…")
        server.start()
        results, wall = run_phase(port, students, workers)
        errors = [e for item in results for e in item["errors"]]
        print(
            f"  墙钟 {wall:.1f}s；"
            + "；".join(
                summarize(k, merge_latencies(results, k)) for k in ("login", "paper", "save")
            )
        )
        if errors:
            failures += errors[:10]
        tokens = {item["student"]: item.get("token", "") for item in results}

        # ---- 阶段 2：进程崩溃 + 重启恢复 ----
        tokens = {item["student"]: item.get("token", "") for item in results}
        if not args.no_restart:
            print("\n[2/4] kill -9（模拟教师机崩溃）→ 重启 → 重登核对未交卷作答…")
            server.crash()
            server.start()
            lost, fresh_tokens = recheck_after_restart(port, results)
            if lost:
                failures += lost[:10]
                print(f"  ✗ 有 {len(lost)} 个学生重启后丢答案")
            else:
                print("  ✓ 全部学生重启后作答完好（未交卷也在）")
            # 重启后旧 token 全失效，交卷必须用重登换来的新 token。
            tokens = fresh_tokens

        # ---- 阶段 3：并发交卷 ----
        print("\n[3/4] 并发交卷…")
        submit_lat, submit_errors = submit_all(port, students, tokens, workers)
        print("  " + summarize("submit", submit_lat[:-1]))
        if submit_errors:
            failures += submit_errors[:10]

        # ---- 阶段 4：收卡核对 ----
        print("\n[4/4] 收卡核对（.eea 数量 / 事件条数 / 状态文件 / verify-answer-sheet）…")
        sheets = sorted(answers_dir.glob("*.eea"))
        events, malformed = count_events(answers_dir)
        states = (
            sorted((answers_dir / "state").glob("*.json"))
            if (answers_dir / "state").is_dir()
            else []
        )
        print(
            f"  .eea={len(sheets)}  submit 事件={events}（坏行 {malformed}）"
            f"  状态文件={len(states)}"
        )
        if len(sheets) != len(students):
            failures.append(f".eea 数量不对：期望 {len(students)}，实际 {len(sheets)}")
        if events != len(students):
            failures.append(f"submit 事件数不对：期望 {len(students)}，实际 {events}")
        if malformed:
            failures.append(f"事件日志有 {malformed} 行无法解析（并发写发生了交错）")
        verify_failures = verify_sheets(builder_exe, package, master, answers_dir)
        if verify_failures:
            failures += verify_failures[:10]
        elif not sheets:
            # 空集合时 verify_sheets 返回空列表；不单独判就会打印「✓ 0 份全部通过」，
            # 人工读日志会被这行误导（虽然后面的数量核对会让退出码正确）。
            failures.append("没有生成任何答题卡（交卷阶段全部失败？）")
        else:
            print(f"  ✓ {len(sheets)} 份答题卡全部通过 verify-answer-sheet")
    finally:
        server.stop()

    print()
    if failures:
        print(f"✗ 演练失败：{len(failures)} 个问题")
        for item in failures[:20]:
            print("  -", item)
        server.tail_log()
        return 1
    # 用 --no-restart 跑时不能宣称「崩溃后可恢复」——那一阶段根本没执行。
    if args.no_restart:
        print("✓ 演练通过：并发请求无错误、答题卡齐全且校验通过（本次跳过了崩溃恢复阶段）")
    else:
        print("✓ 演练通过：并发请求无错误、崩溃后可恢复、答题卡齐全且校验通过")
    return 0


if __name__ == "__main__":
    sys.exit(main())
