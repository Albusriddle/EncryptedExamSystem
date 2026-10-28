"""RFC3339 时间解析。

与 Rust 端 `common::timegate::parse_rfc3339` 行为保持一致：
支持 `2026-12-20T09:00:00+08:00`、`...Z`、以及用空格代替 T。

不引入 `datetime.datetime.fromisoformat` 的原因：Python 3.11 之前的
`fromisoformat` 对 `Z` 后缀和部分偏移格式支持不全，而这个项目要
在教师机器上稳定运行，不能依赖版本细节。
"""

from __future__ import annotations


class TimeParseError(ValueError):
    """时间格式无法解析。"""


def parse_rfc3339(s: str) -> int:
    """解析为 Unix 时间戳（秒，UTC）。失败抛 `TimeParseError`。"""
    text = s.strip()
    if not text:
        raise TimeParseError("时间为空")
    if not text.isascii():
        raise TimeParseError("时间只能使用 ASCII 数字/符号")

    # 日期与时间分隔
    if "T" in text:
        date_part, rest = text.split("T", 1)
    elif " " in text:
        date_part, rest = text.split(" ", 1)
    else:
        raise TimeParseError(f"缺少日期与时间的分隔符: {s!r}")

    d = date_part.split("-")
    if len(d) != 3:
        raise TimeParseError(f"日期格式错误: {date_part!r}")
    try:
        year, month, day = int(d[0]), int(d[1]), int(d[2])
    except ValueError as exc:
        raise TimeParseError(f"日期含非数字: {date_part!r}") from exc
    if not (1 <= month <= 12) or not (1 <= day <= 31):
        raise TimeParseError(f"月/日超出范围: {date_part!r}")
    # 日历合法性：只查 1..=31 会把 2026-02-31 静默算成 3 月 3 日。
    # 与 Rust 的 days_in_month 校验保持一致。
    import datetime as _dt

    try:
        _dt.date(year, month, day)
    except ValueError as exc:
        raise TimeParseError(f"日期不存在: {date_part!r}") from exc

    # 时区偏移
    offset = 0
    time_part = rest
    if rest.endswith("Z") or rest.endswith("z"):
        time_part = rest[:-1]
    else:
        # 从右往左找最后一个 + 或 -
        idx = max(rest.rfind("+"), rest.rfind("-"))
        if idx > 0:
            time_part = rest[:idx]
            off = rest[idx:]
            sign = -1 if off.startswith("-") else 1
            body = off[1:]
            if ":" in body:
                oh, om = body.split(":", 1)
            elif len(body) == 4:
                oh, om = body[:2], body[2:]
            else:
                raise TimeParseError(f"时区偏移非法: {off!r}")
            if len(oh) != 2 or len(om) != 2:
                raise TimeParseError(f"时区偏移非法: {off!r}")
            try:
                oh_i, om_i = int(oh), int(om or 0)
                if not (0 <= oh_i <= 14 and 0 <= om_i <= 59):
                    raise ValueError
                if oh_i == 14 and om_i != 0:
                    raise ValueError
                offset = sign * (oh_i * 3600 + om_i * 60)
            except ValueError as exc:
                raise TimeParseError(f"时区偏移非法: {off!r}") from exc

    t = time_part.split(":")
    if len(t) < 2:
        raise TimeParseError(f"时间格式错误: {time_part!r}")
    try:
        hour = int(t[0])
        minute = int(t[1])
        second = int(t[2].split(".")[0]) if len(t) > 2 else 0
    except ValueError as exc:
        raise TimeParseError(f"时间含非数字: {time_part!r}") from exc
    if not (0 <= hour <= 23 and 0 <= minute <= 59 and 0 <= second <= 60):
        raise TimeParseError(f"时分秒超出范围: {time_part!r}")

    days = _days_from_civil(year, month, day)
    return days * 86400 + hour * 3600 + minute * 60 + second - offset


def _days_from_civil(y: int, m: int, d: int) -> int:
    """公历日期 → 距 1970-01-01 的天数（Howard Hinnant 算法）。"""
    if m <= 2:
        y -= 1
    era = (y if y >= 0 else y - 399) // 400
    yoe = y - era * 400
    mp = (m + 9) % 12
    doy = (153 * mp + 2) // 5 + d - 1
    doe = yoe * 365 + yoe // 4 - yoe // 100 + doy
    return era * 146097 + doe - 719468


def format_rfc3339_local(unix: int) -> str:
    """把 Unix 时间戳格式化为本地时间 `YYYY-MM-DD HH:MM`，用于界面显示与编辑。"""
    import datetime as _dt

    return _dt.datetime.fromtimestamp(unix).strftime("%Y-%m-%d %H:%M")


def local_to_rfc3339(text: str) -> str:
    """把界面上的 `YYYY-MM-DD HH:MM` 转成带本地时区偏移的 RFC3339 字符串。

    Rust 端存的是带偏移的绝对时刻，所以这里必须补上偏移，
    不能直接拼 `Z`——否则教师在东八区填 09:00 会被当成 UTC 09:00。
    """
    import datetime as _dt

    dt = _dt.datetime.strptime(text.strip(), "%Y-%m-%d %H:%M")
    # astimezone() 会给本地时区；去掉微秒
    local = dt.astimezone()
    return local.strftime("%Y-%m-%dT%H:%M:%S%z")[:-2] + ":" + local.strftime("%z")[-2:]
