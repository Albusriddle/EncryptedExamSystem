//! 网络时间门禁（S-2）。
//!
//! 设计要点：
//!
//! 1. **多源取中位数**：并发请求多个 HTTPS 时间源，任一单点被劫持都无法
//!    独自决定结果。
//! 2. **fail-closed**：成功源少于 2 个、或各源分歧超过阈值，一律拒绝放行。
//!    宁可误锁，不可误放——这是考试系统的正确默认值。
//! 3. **不信任本机时钟**：只把本机时间当作参考，偏差过大时给出警告，
//!    但判定完全基于网络时间。
//! 4. **可注入**：`TimeSource` 是 trait，单元测试用假时间源，不碰网络。
//!
//! ## 为什么用 WinHTTP 而不是 ureq/reqwest
//!
//! 那两个库的 TLS 后端（ring / aws-lc-rs）需要 C 编译器与完整 UCRT 头文件，
//! 在只装了部分 VS Build Tools 的机器上会编译失败。WinHTTP 是 Windows 自带
//! 组件：零 C 编译依赖、证书校验由系统负责、二进制不膨胀。
//! 非 Windows 平台用 curl 兜底（CI 在 Linux 上跑）。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 时间获取错误。
#[derive(Debug, thiserror::Error)]
pub enum TimeError {
    #[error("可用的时间源不足（成功 {ok} 个，至少需要 {need} 个）")]
    NotEnoughSources { ok: usize, need: usize },
    #[error("各时间源分歧过大（{spread} 秒 > {limit} 秒），疑似网络劫持")]
    SourcesDisagree { spread: u64, limit: u64 },
    #[error("全部时间源均失败: {0}")]
    AllFailed(String),
}

/// 单个时间源。
pub trait TimeSource: Send + Sync {
    /// 返回该源报告的当前 Unix 时间戳（秒）。
    fn now_unix(&self) -> Result<u64, String>;
    /// 用于日志的名称。
    fn name(&self) -> &str;
}

/// 通过 HTTPS 响应头 `Date` 取时。
///
/// 选 HTTPS 而非 NTP 的原因：校园网/机房普遍屏蔽 UDP 123，
/// 而 443 只要"能上网"就通。
pub struct HttpsDateSource {
    pub url: String,
    pub name: String,
    pub timeout: Duration,
}

impl HttpsDateSource {
    pub fn new(url: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            name: name.into(),
            timeout: Duration::from_secs(3),
        }
    }
}

impl TimeSource for HttpsDateSource {
    fn now_unix(&self) -> Result<u64, String> {
        let date = fetch_date_header(&self.url, self.timeout)
            .map_err(|e| format!("{} 请求失败: {e}", self.name))?;
        parse_http_date(&date).ok_or_else(|| format!("{} Date 头无法解析: {date}", self.name))
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// 发一个 HTTPS GET，只取 `Date` 响应头。
#[cfg(windows)]
fn fetch_date_header(url: &str, timeout: Duration) -> Result<String, String> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Networking::WinHttp::*;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    unsafe {
        let agent = wide("EncryptedExamSystem/0.1");
        let session = WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            std::ptr::null(),
            std::ptr::null(),
            0,
        );
        if session.is_null() {
            return Err(format!("WinHttpOpen 失败（错误码 {}）", GetLastError()));
        }
        // 确保会话句柄在任何返回路径上都被关闭。
        let _guard = SessionGuard(session);

        let ms = timeout.as_millis().min(u32::MAX as u128) as i32;
        WinHttpSetTimeouts(session, ms, ms, ms, ms);

        // 拆分 URL：https://host/path
        let rest = url
            .strip_prefix("https://")
            .ok_or_else(|| format!("只支持 https:// 开头的地址: {url}"))?;
        let (host, path) = match rest.split_once('/') {
            Some((h, p)) => (h, format!("/{p}")),
            None => (rest, "/".to_string()),
        };

        let host_w = wide(host);
        let path_w = wide(&path);
        let connect = WinHttpConnect(session, host_w.as_ptr(), 443, 0);
        if connect.is_null() {
            return Err(format!("WinHttpConnect 失败（错误码 {}）", GetLastError()));
        }
        let _connect_guard = HandleGuard(connect);

        let request = WinHttpOpenRequest(
            connect,
            wide("GET").as_ptr(),
            path_w.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        );
        if request.is_null() {
            return Err(format!(
                "WinHttpOpenRequest 失败（错误码 {}）",
                GetLastError()
            ));
        }
        let _req_guard = HandleGuard(request);

        if WinHttpSendRequest(request, std::ptr::null(), 0, std::ptr::null_mut(), 0, 0, 0) == 0 {
            return Err(format!(
                "WinHttpSendRequest 失败（错误码 {}）",
                GetLastError()
            ));
        }
        if WinHttpReceiveResponse(request, std::ptr::null_mut()) == 0 {
            return Err(format!(
                "WinHttpReceiveResponse 失败（错误码 {}）",
                GetLastError()
            ));
        }

        // 先查缓冲区大小，再读。
        let mut size: u32 = 0;
        WinHttpQueryHeaders(
            request,
            WINHTTP_QUERY_DATE,
            std::ptr::null(),
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
        );
        if size == 0 {
            return Err("响应中没有 Date 头".into());
        }
        let mut buf: Vec<u16> = vec![0; (size as usize / 2) + 1];
        if WinHttpQueryHeaders(
            request,
            WINHTTP_QUERY_DATE,
            std::ptr::null(),
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(format!("读取 Date 头失败（错误码 {}）", GetLastError()));
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16(&buf[..end]).map_err(|e| format!("Date 头解码失败: {e}"))
    }
}

#[cfg(windows)]
struct SessionGuard(*mut core::ffi::c_void);
#[cfg(windows)]
impl Drop for SessionGuard {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Networking::WinHttp::WinHttpCloseHandle(self.0) };
    }
}

#[cfg(windows)]
struct HandleGuard(*mut core::ffi::c_void);
#[cfg(windows)]
impl Drop for HandleGuard {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Networking::WinHttp::WinHttpCloseHandle(self.0) };
    }
}

/// 非 Windows：用 curl 兜底（CI 在 Linux 上跑）。
#[cfg(not(windows))]
fn fetch_date_header(url: &str, timeout: Duration) -> Result<String, String> {
    let out = std::process::Command::new("curl")
        .args(["-sSI", "--max-time", &timeout.as_secs().to_string(), url])
        .output()
        .map_err(|e| format!("无法执行 curl: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("date:")
            && let Some(v) = line.get(5..)
        {
            let _ = rest;
            return Ok(v.trim().to_string());
        }
        if line.len() > 5 && line[..5].eq_ignore_ascii_case("date:") {
            return Ok(line[5..].trim().to_string());
        }
    }
    Err("响应中没有 Date 头".into())
}

/// 解析 HTTP `Date` 头（RFC 7231 IMF-fixdate）。
fn parse_http_date(s: &str) -> Option<u64> {
    let s = s.trim();
    let rest = s.split_once(", ").map(|(_, r)| r).unwrap_or(s);
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() < 5 {
        return None;
    }
    let day: u64 = parts[0].parse().ok()?;
    let month = match parts[1] {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: u64 = parts[2].parse().ok()?;
    let time: Vec<&str> = parts[3].split(':').collect();
    if time.len() != 3 {
        return None;
    }
    let (h, m, sec): (u64, u64, u64) = (
        time[0].parse().ok()?,
        time[1].parse().ok()?,
        time[2].parse().ok()?,
    );
    Some(days_from_civil(year as i64, month as i64, day as i64) * 86400 + h * 3600 + m * 60 + sec)
}

/// 公历日期 → 距 1970-01-01 的天数（Howard Hinnant 算法）。
fn days_from_civil(y: i64, m: i64, d: i64) -> u64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    days as u64
}

/// 当前时间的判定结果。
#[derive(Debug, Clone)]
pub struct NetworkTime {
    /// 中位数时间（Unix 秒）
    pub unix: u64,
    /// 参与计算的源数量
    pub sources: usize,
    /// 各源最大分歧（秒）
    pub spread: u64,
    /// 与本机时钟的偏差（秒，带符号）
    pub local_skew_secs: i64,
}

/// 聚合多个时间源。
///
/// `min_sources` 是放行所需的最少成功源数（建议 2）。
/// `max_spread` 是允许的最大分歧（建议 300 秒）。
pub fn fetch_network_time(
    sources: &[Box<dyn TimeSource>],
    min_sources: usize,
    max_spread: u64,
) -> Result<NetworkTime, TimeError> {
    let mut ok: Vec<u64> = Vec::new();
    let mut errs: Vec<String> = Vec::new();
    for src in sources {
        match src.now_unix() {
            Ok(t) => ok.push(t),
            Err(e) => errs.push(e),
        }
    }

    if ok.len() < min_sources {
        if errs.len() == sources.len() && !sources.is_empty() {
            return Err(TimeError::AllFailed(errs.join("; ")));
        }
        return Err(TimeError::NotEnoughSources {
            ok: ok.len(),
            need: min_sources,
        });
    }

    ok.sort_unstable();
    let median = ok[ok.len() / 2];
    let spread = ok[ok.len() - 1] - ok[0];
    if spread > max_spread {
        return Err(TimeError::SourcesDisagree {
            spread,
            limit: max_spread,
        });
    }

    let local = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let local_skew_secs = local as i64 - median as i64;

    Ok(NetworkTime {
        unix: median,
        sources: ok.len(),
        spread,
        local_skew_secs,
    })
}

/// 把 Unix 时间戳格式化为 `YYYY-MM-DD HH:MM:SS`（UTC），用于日志与提示。
pub fn format_unix_utc(unix: u64) -> String {
    let days = (unix / 86400) as i64;
    let secs = unix % 86400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// 天数 → 公历日期（Howard Hinnant 算法）。
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeSource {
        name: String,
        value: Result<u64, String>,
    }

    impl TimeSource for FakeSource {
        fn now_unix(&self) -> Result<u64, String> {
            self.value.clone()
        }
        fn name(&self) -> &str {
            &self.name
        }
    }

    fn src(name: &str, v: Result<u64, String>) -> Box<dyn TimeSource> {
        Box::new(FakeSource {
            name: name.into(),
            value: v,
        })
    }

    #[test]
    fn median_of_three() {
        let sources = vec![src("a", Ok(1000)), src("b", Ok(1002)), src("c", Ok(1001))];
        let t = fetch_network_time(&sources, 2, 300).unwrap();
        assert_eq!(t.unix, 1001);
        assert_eq!(t.sources, 3);
        assert_eq!(t.spread, 2);
    }

    #[test]
    fn fails_closed_when_not_enough_sources() {
        let sources = vec![src("a", Ok(1000)), src("b", Err("down".into()))];
        let err = fetch_network_time(&sources, 2, 300).unwrap_err();
        assert!(matches!(
            err,
            TimeError::NotEnoughSources { ok: 1, need: 2 }
        ));
    }

    #[test]
    fn fails_closed_on_large_spread() {
        let sources = vec![src("a", Ok(1000)), src("b", Ok(5000)), src("c", Ok(1001))];
        let err = fetch_network_time(&sources, 2, 300).unwrap_err();
        assert!(matches!(err, TimeError::SourcesDisagree { .. }));
    }

    #[test]
    fn all_failed_reports_reason() {
        let sources = vec![src("a", Err("x".into())), src("b", Err("y".into()))];
        let err = fetch_network_time(&sources, 2, 300).unwrap_err();
        assert!(matches!(err, TimeError::AllFailed(_)));
    }

    #[test]
    fn parse_standard_http_date() {
        assert_eq!(
            parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(784111777)
        );
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_http_date("").is_none());
        assert!(parse_http_date("not a date").is_none());
        assert!(parse_http_date("Sun, 06 Xxx 1994 08:49:37 GMT").is_none());
    }

    #[test]
    fn civil_roundtrip() {
        assert_eq!(format_unix_utc(0), "1970-01-01 00:00:00");
        assert_eq!(format_unix_utc(784111777), "1994-11-06 08:49:37");
    }
}
