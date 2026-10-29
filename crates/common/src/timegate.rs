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

use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;

use crate::model::TimeConfig;

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
        if line
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("date:"))
        {
            return Ok(line.get(5..).unwrap_or("").trim().to_string());
        }
    }
    Err("响应中没有 Date 头".into())
}

/// 读取 HTTPS 响应正文（签名时间令牌用）。
#[cfg(windows)]
fn fetch_body(url: &str, timeout: Duration) -> Result<String, String> {
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
        let _guard = SessionGuard(session);
        let ms = timeout.as_millis().min(u32::MAX as u128) as i32;
        WinHttpSetTimeouts(session, ms, ms, ms, ms);

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
        let mut status: u32 = 0;
        let mut status_len: u32 = std::mem::size_of::<u32>() as u32;
        let status_ok = WinHttpQueryHeaders(
            request,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            std::ptr::null(),
            (&mut status as *mut u32).cast(),
            &mut status_len,
            std::ptr::null_mut(),
        );
        if status_ok == 0 {
            return Err(format!("读取 HTTP 状态码失败（错误码 {}）", GetLastError()));
        }
        if !(200..300).contains(&status) {
            return Err(format!("时间服务 HTTP {status}"));
        }
        let mut body = Vec::new();
        loop {
            let mut buf = [0u8; 4096];
            let mut read: u32 = 0;
            if WinHttpReadData(
                request,
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
                &mut read,
            ) == 0
            {
                return Err(format!("读取响应失败（错误码 {}）", GetLastError()));
            }
            if read == 0 {
                break;
            }
            body.extend_from_slice(&buf[..read as usize]);
            if body.len() > 1_000_000 {
                return Err("时间令牌响应过大".into());
            }
        }
        String::from_utf8(body).map_err(|e| format!("响应不是 UTF-8: {e}"))
    }
}

#[cfg(not(windows))]
fn fetch_body(url: &str, timeout: Duration) -> Result<String, String> {
    let out = std::process::Command::new("curl")
        .args([
            "-fsSL",
            "--max-filesize",
            "1000000",
            "--max-time",
            &timeout.as_secs().to_string(),
            url,
        ])
        .output()
        .map_err(|e| format!("启动 curl 失败: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "curl 请求失败: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("响应不是 UTF-8: {e}"))
}

#[derive(Deserialize)]
struct SignedToken {
    t: u64,
    n: String,
    sig: String,
}

/// 验证服务端返回的令牌正文（独立函数便于单元测试）。
pub fn verify_signed_token(config: &TimeConfig, body: &str, nonce: &str) -> Result<u64, String> {
    let token: SignedToken =
        serde_json::from_str(body).map_err(|e| format!("时间令牌 JSON 无法解析: {e}"))?;
    if token.n != nonce {
        return Err("时间令牌 nonce 不匹配（疑似重放）".into());
    }
    let pub_raw = base64::engine::general_purpose::STANDARD
        .decode(&config.pubkey)
        .map_err(|_| "时间令牌公钥不是合法 base64".to_string())?;
    let pub_arr: [u8; 32] = pub_raw
        .as_slice()
        .try_into()
        .map_err(|_| "时间令牌公钥长度不是 32 字节".to_string())?;
    let key = VerifyingKey::from_bytes(&pub_arr).map_err(|e| format!("时间令牌公钥无效: {e}"))?;
    let sig_raw = base64::engine::general_purpose::STANDARD
        .decode(&token.sig)
        .map_err(|_| "时间令牌签名不是合法 base64".to_string())?;
    let sig =
        Signature::from_slice(&sig_raw).map_err(|_| "时间令牌签名长度不是 64 字节".to_string())?;
    key.verify(format!("{}:{}", token.t, token.n).as_bytes(), &sig)
        .map_err(|_| "时间令牌签名验证失败".to_string())?;
    Ok(token.t)
}

fn make_nonce() -> String {
    // 16 字节 = 32 位 hex，与协议文档/服务端示例一致。
    hex::encode(&crate::crypto::random_key()[..16])
}

/// 获取并验证签名时间令牌。签名覆盖 `"{t}:{nonce}"`。
pub fn fetch_signed_time(config: &TimeConfig) -> Result<NetworkTime, String> {
    config.validate()?;
    let nonce = make_nonce();
    let sep = if config.url.contains('?') { '&' } else { '?' };
    let url = format!("{}{}nonce={}", config.url, sep, nonce);
    let body = fetch_body(&url, Duration::from_secs(3))?;
    let t = verify_signed_token(config, &body, &nonce)?;

    let local = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let skew = local as i64 - t as i64;
    if skew.unsigned_abs() > config.max_skew {
        eprintln!(
            "⚠ 本机时钟与签名时间相差 {skew} 秒（配置允许 {} 秒）",
            config.max_skew
        );
    }
    Ok(NetworkTime {
        unix: t,
        sources: 1,
        spread: 0,
        local_skew_secs: skew,
    })
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

/// 某年某月的天数（含闰年规则）。
fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
            if leap { 29 } else { 28 }
        }
        _ => 0,
    }
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
/// 可用源数量决定了分歧容差。
///
/// - **3 个及以上**：中位数能吃掉一个撒谎的源，容差放宽到调用方给的上限（正常是 60 秒）。
/// - **2 个**：`ok[len/2]` 取的是较大的那个，等于直接采信"更晚"的时间，
///   任何分歧都会全量进入结果——只允许 10 秒。实测正常网络下三源分歧为 0 秒，
///   所以这个上限不会误伤。
///
/// 之所以不用"两个源取平均"：平均同样会被单个坏源拉动，而且方向不可控；
/// 宁可让 2 源时更容易 fail-closed（学生重试一次通常就能凑齐 3 源）。
pub fn spread_limit(ok_sources: usize, max_spread: u64) -> u64 {
    const TWO_SOURCE_MAX_SPREAD: u64 = 10;
    if ok_sources <= 2 {
        max_spread.min(TWO_SOURCE_MAX_SPREAD)
    } else {
        max_spread
    }
}

/// `max_spread` 是允许的最大分歧（建议 60 秒）。
///
/// 实际生效的上限还取决于**可用源数量**（见 [`spread_limit`]）：
/// 2 个源时中位数退化成"较大的那个"，任何一个源的偏差都会原样进结果，
/// 所以只能容忍极小分歧。
pub fn fetch_network_time(
    sources: &[Box<dyn TimeSource>],
    min_sources: usize,
    max_spread: u64,
) -> Result<NetworkTime, TimeError> {
    fetch_network_time_with_policy(
        sources,
        FetchPolicy {
            min_sources,
            max_spread,
            rounds: 1,
        },
    )
}

/// 取时策略。
#[derive(Debug, Clone, Copy)]
pub struct FetchPolicy {
    pub min_sources: usize,
    pub max_spread: u64,
    /// 最多采几轮（1 = 不重试）。
    ///
    /// 只有"可用源不足"时才继续下一轮，且**只重查失败的那些源**——
    /// 已经成功的源不再查询，重试成本因此只有一次超时（约 3 秒）。
    /// 考场 Wi-Fi 抖一下就整场考不了，代价远高于这 3 秒。
    pub rounds: u32,
}

impl Default for FetchPolicy {
    fn default() -> Self {
        Self {
            min_sources: 2,
            max_spread: 60,
            rounds: 2,
        }
    }
}

/// 并发取一次时（每个源一个线程）。
///
/// 串行取 3 个源时，最坏要等 3×3 秒才失败；并发之后单轮最坏 3 秒。
/// 线程崩溃（源内部 panic）按该源失败处理，不影响其它源。
fn sample_parallel(
    sources: &[Box<dyn TimeSource>],
    pending: &[usize],
) -> Vec<(usize, Result<u64, String>)> {
    std::thread::scope(|scope| {
        let handles: Vec<(usize, _)> = pending
            .iter()
            .map(|&i| (i, scope.spawn(move || sources[i].now_unix())))
            .collect();
        handles
            .into_iter()
            .map(|(i, h)| (i, h.join().unwrap_or_else(|_| Err("取时线程崩溃".into()))))
            .collect()
    })
}

/// 带重试的取时。
pub fn fetch_network_time_with_policy(
    sources: &[Box<dyn TimeSource>],
    policy: FetchPolicy,
) -> Result<NetworkTime, TimeError> {
    let n = sources.len();
    let mut values: Vec<Option<u64>> = vec![None; n];
    let mut errors: Vec<Option<String>> = vec![None; n];
    let mut pending: Vec<usize> = (0..n).collect();

    for _ in 0..policy.rounds.max(1) {
        if pending.is_empty() {
            break;
        }
        for (idx, res) in sample_parallel(sources, &pending) {
            match res {
                Ok(t) => {
                    values[idx] = Some(t);
                    errors[idx] = None;
                }
                Err(e) => errors[idx] = Some(e),
            }
        }
        if values.iter().filter(|v| v.is_some()).count() >= policy.min_sources {
            break; // 够用了，不再等失败源
        }
        pending = values
            .iter()
            .enumerate()
            .filter(|(_, v)| v.is_none())
            .map(|(i, _)| i)
            .collect();
    }

    let mut ok: Vec<u64> = values.iter().flatten().copied().collect();
    // min_sources=0 时也至少要有 1 个源，否则下面的中位数索引会空数组 panic。
    if ok.len() < policy.min_sources.max(1) {
        let details: Vec<String> = errors
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.as_ref().map(|e| format!("{}: {e}", sources[i].name())))
            .collect();
        if ok.is_empty() && !sources.is_empty() {
            return Err(TimeError::AllFailed(details.join("; ")));
        }
        return Err(TimeError::NotEnoughSources {
            ok: ok.len(),
            need: policy.min_sources,
        });
    }

    ok.sort_unstable();
    let median = ok[ok.len() / 2];
    let spread = ok[ok.len() - 1] - ok[0];
    let limit = spread_limit(ok.len(), policy.max_spread);
    if spread > limit {
        return Err(TimeError::SourcesDisagree { spread, limit });
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

/// 把 Unix 时间戳格式化为 `YYYY-MM-DD HH:MM:SS`（UTC）。
///
/// 注意：给**学生看的提示**不要用它——学生会看到 UTC 而不是自己的时间。
/// 用 [`format_unix_local_zoned`]。
pub fn format_unix_utc(unix: u64) -> String {
    format_unix_at_offset(unix, 0)
}

/// 按指定时区偏移（秒）格式化。纯函数，便于跨时区测试。
pub fn format_unix_at_offset(unix: u64, offset_secs: i32) -> String {
    let shifted = unix as i64 + offset_secs as i64;
    let days = shifted.div_euclid(86400);
    let secs = shifted.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// 时区标签，例如 `UTC+8`、`UTC-5`、`UTC`。
pub fn offset_label(offset_secs: i32) -> String {
    if offset_secs == 0 {
        return "UTC".to_string();
    }
    let sign = if offset_secs > 0 { '+' } else { '-' };
    let a = offset_secs.abs();
    let h = a / 3600;
    let m = (a % 3600) / 60;
    if m == 0 {
        format!("UTC{sign}{h}")
    } else {
        format!("UTC{sign}{h}:{m:02}")
    }
}

/// 本机时区相对 UTC 的偏移秒数（东八区 = +28800）。
///
/// Windows 走 `GetTimeZoneInformation`（含夏令时修正）；
/// 其他平台返回 0（按 UTC 显示）——学生端只发 Windows，够用。
#[cfg(windows)]
pub fn local_offset_secs() -> i32 {
    use windows_sys::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
    // TIME_ZONE_ID_STANDARD = 1, TIME_ZONE_ID_DAYLIGHT = 2
    unsafe {
        let mut tzi: TIME_ZONE_INFORMATION = std::mem::zeroed();
        let id = GetTimeZoneInformation(&mut tzi);
        let bias_min = match id {
            1 => tzi.Bias + tzi.StandardBias,
            2 => tzi.Bias + tzi.DaylightBias,
            _ => tzi.Bias,
        };
        // Windows 的 Bias 定义是 UTC = local + Bias，所以要取反
        -(bias_min * 60)
    }
}

/// 非 Windows：按 UTC 显示（偏移 0）。
#[cfg(not(windows))]
pub fn local_offset_secs() -> i32 {
    0
}

/// 格式化为**本机时间**并标注时区，例如 `2026-10-22 18:28:57（UTC+8）`。
///
/// 学生看到的必须是自己的时间；只写 UTC 会让人以为程序算错了时间。
pub fn format_unix_local_zoned(unix: u64) -> String {
    let off = local_offset_secs();
    format!(
        "{}（{}）",
        format_unix_at_offset(unix, off),
        offset_label(off)
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
        let t = fetch_network_time(&sources, 2, 60).unwrap();
        assert_eq!(t.unix, 1001);
        assert_eq!(t.sources, 3);
        assert_eq!(t.spread, 2);
    }

    #[test]
    fn fails_closed_when_not_enough_sources() {
        let sources = vec![src("a", Ok(1000)), src("b", Err("down".into()))];
        let err = fetch_network_time(&sources, 2, 60).unwrap_err();
        assert!(matches!(
            err,
            TimeError::NotEnoughSources { ok: 1, need: 2 }
        ));
    }

    #[test]
    fn fails_closed_on_large_spread() {
        let sources = vec![src("a", Ok(1000)), src("b", Ok(5000)), src("c", Ok(1001))];
        let err = fetch_network_time(&sources, 2, 60).unwrap_err();
        assert!(matches!(err, TimeError::SourcesDisagree { .. }));
    }

    /// 2 个源时只容忍 10 秒分歧：中位数退化为"较大的那个"，
    /// 20 秒的偏差会原样进结果，必须拒绝。
    #[test]
    fn two_sources_require_tight_agreement() {
        let sources = vec![src("a", Ok(1000)), src("b", Ok(1020))];
        let err = fetch_network_time(&sources, 2, 60).unwrap_err();
        assert!(
            matches!(
                err,
                TimeError::SourcesDisagree {
                    spread: 20,
                    limit: 10
                }
            ),
            "2 源 20 秒分歧应被拒绝，实际: {err:?}"
        );

        // 10 秒以内可以接受
        let ok = vec![src("a", Ok(1000)), src("b", Ok(1010))];
        let t = fetch_network_time(&ok, 2, 60).unwrap();
        assert_eq!(t.spread, 10);
        // 取上中位数：结果是有利于"更晚"的那个
        assert_eq!(t.unix, 1010);
    }

    /// 3 个源时中位数能吃掉一个坏源：即便某个源偏 20 秒也不影响结果。
    #[test]
    fn three_sources_tolerate_one_outlier() {
        let sources = vec![src("a", Ok(1000)), src("b", Ok(1020)), src("c", Ok(1001))];
        let t = fetch_network_time(&sources, 2, 60).unwrap();
        assert_eq!(t.unix, 1001, "中位数应取 1001，而不是被 1020 拉动");
        assert_eq!(t.spread, 20);
    }

    /// 会失败的源：前 `fail_times` 次报错，之后返回 `value`。
    /// 返回 (可装箱的源, 调用计数器) —— 计数器用来断言"重试了几次"。
    struct FlakySource {
        name: String,
        value: u64,
        fail_times: std::sync::atomic::AtomicUsize,
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl FlakySource {
        /// 返回 (可装箱的源, 调用计数器)——不叫 new，因为它返回的不是 Self。
        fn make(
            name: &str,
            value: u64,
            fail_times: usize,
        ) -> (
            Box<dyn TimeSource>,
            std::sync::Arc<std::sync::atomic::AtomicUsize>,
        ) {
            let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let src = Box::new(Self {
                name: name.into(),
                value,
                fail_times: std::sync::atomic::AtomicUsize::new(fail_times),
                calls: std::sync::Arc::clone(&calls),
            });
            (src, calls)
        }
    }

    impl TimeSource for FlakySource {
        fn now_unix(&self) -> Result<u64, String> {
            use std::sync::atomic::Ordering;
            self.calls.fetch_add(1, Ordering::SeqCst);
            // 手写"取一次并减一"：原子 API 在不同版本间改过名，
            // 用 compare_exchange 循环最稳，也不引入版本差异。
            let mut remaining = self.fail_times.load(Ordering::SeqCst);
            while remaining > 0 {
                match self.fail_times.compare_exchange(
                    remaining,
                    remaining - 1,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => return Err("暂时不可用".into()),
                    Err(actual) => remaining = actual,
                }
            }
            Ok(self.value)
        }
        fn name(&self) -> &str {
            &self.name
        }
    }

    /// 一次网络抖动不应让学生考不了：失败的源会被重试。
    #[test]
    fn retry_recovers_transient_failure() {
        let (flaky, calls) = FlakySource::make("flaky", 1000, 1);
        let sources = vec![src("a", Ok(1000)), flaky];
        let policy = FetchPolicy {
            min_sources: 2,
            max_spread: 60,
            rounds: 2,
        };
        let t = fetch_network_time_with_policy(&sources, policy).unwrap();
        assert_eq!(t.unix, 1000);
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "失败源应被重试一次"
        );
    }

    /// 彻底失败时仍然 fail-closed，并且只重试有限轮。
    #[test]
    fn retry_exhausted_still_fails_closed() {
        let (dead, calls) = FlakySource::make("dead", 1000, 99);
        let sources = vec![src("a", Ok(1000)), dead];
        let policy = FetchPolicy {
            min_sources: 2,
            max_spread: 60,
            rounds: 2,
        };
        let err = fetch_network_time_with_policy(&sources, policy).unwrap_err();
        assert!(matches!(
            err,
            TimeError::NotEnoughSources { ok: 1, need: 2 }
        ));
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "最多重试到 rounds 次"
        );
    }

    /// 源够用时不该有任何重试（不增加正常路径的延迟）。
    #[test]
    fn no_retry_when_enough_sources() {
        let sources = vec![src("a", Ok(1000)), src("b", Ok(1001)), src("c", Ok(1002))];
        let policy = FetchPolicy {
            min_sources: 2,
            max_spread: 60,
            rounds: 3,
        };
        let t = fetch_network_time_with_policy(&sources, policy).unwrap();
        assert_eq!(t.unix, 1001);
        assert_eq!(t.sources, 3);
    }

    #[test]
    fn spread_limit_by_source_count() {
        assert_eq!(spread_limit(2, 60), 10);
        assert_eq!(spread_limit(3, 60), 60);
        assert_eq!(spread_limit(4, 60), 60);
        // 调用方给的上限比 10 秒还小时，取更严的那个
        assert_eq!(spread_limit(2, 5), 5);
    }

    #[test]
    fn all_failed_reports_reason() {
        let sources = vec![src("a", Err("x".into())), src("b", Err("y".into()))];
        let err = fetch_network_time(&sources, 2, 60).unwrap_err();
        assert!(matches!(err, TimeError::AllFailed(_)));
    }

    /// 日历合法性：2 月 31 日不能被静默算成 3 月 3 日。
    #[test]
    fn parse_rejects_nonexistent_calendar_dates() {
        assert_eq!(parse_rfc3339("2026-02-31T10:00:00+08:00"), None);
        assert_eq!(parse_rfc3339("2026-04-31T10:00:00+08:00"), None);
        assert_eq!(
            parse_rfc3339("2026-02-29T10:00:00+08:00"),
            None,
            "2026 不是闰年"
        );
        // 闰年 2 月 29 必须正常
        assert!(parse_rfc3339("2024-02-29T10:00:00+08:00").is_some());
        // 月末合法日期
        assert!(parse_rfc3339("2026-01-31T10:00:00+08:00").is_some());
        assert!(parse_rfc3339("2026-12-31T10:00:00+08:00").is_some());
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

    /// 东八区：UTC 时间 +8 小时，日期可能跨天。
    #[test]
    fn format_at_offset_shifts_correctly() {
        let t = 1_704_067_200; // 2024-01-01 00:00:00 UTC
        assert_eq!(format_unix_at_offset(t, 0), "2024-01-01 00:00:00");
        assert_eq!(format_unix_at_offset(t, 8 * 3600), "2024-01-01 08:00:00");
        assert_eq!(format_unix_at_offset(t, -5 * 3600), "2023-12-31 19:00:00");
    }

    /// 负偏移不能因取模写出错误日期。
    #[test]
    fn format_at_offset_negative_offset_keeps_date_valid() {
        let t = 1_704_067_200; // 2024-01-01
        assert_eq!(
            format_unix_at_offset(t, -(12 * 3600)),
            "2023-12-31 12:00:00"
        );
    }

    #[test]
    fn offset_labels() {
        assert_eq!(offset_label(0), "UTC");
        assert_eq!(offset_label(8 * 3600), "UTC+8");
        assert_eq!(offset_label(-5 * 3600), "UTC-5");
        assert_eq!(offset_label(5 * 3600 + 30 * 60), "UTC+5:30");
        assert_eq!(offset_label(-(3 * 3600 + 30 * 60)), "UTC-3:30");
    }

    /// 本机偏移必须落在现实范围内，否则说明系统调用取错了。
    #[test]
    fn local_offset_is_plausible() {
        let off = local_offset_secs();
        assert!(
            (-12 * 3600..=14 * 3600).contains(&off),
            "本机时区偏移异常: {off} 秒"
        );
        assert_eq!(off % 900, 0, "偏移应是 15 分钟的整数倍，实际 {off}");
    }

    /// 学生看到的提示必须带时区标签，不能是裸 UTC。
    #[test]
    fn zoned_format_includes_offset() {
        let s = format_unix_local_zoned(1_704_067_200);
        assert!(s.contains("（UTC"), "缺少时区标注: {s}");
    }

    #[test]
    fn civil_roundtrip() {
        assert_eq!(format_unix_utc(0), "1970-01-01 00:00:00");
        assert_eq!(format_unix_utc(784111777), "1994-11-06 08:49:37");
    }
}

// ============================================================================
// 场次门禁判定（纯函数，可离线单测）
// ============================================================================

/// 门禁拒绝的原因。**必须可区分**，否则学生无从判断是自己输错学号还是时间没到。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GateError {
    #[error("考试尚未开始。当前网络时间 {now}，最早一场开始于 {first}")]
    NotStarted { now: String, first: String },
    #[error("当前网络时间 {now}，不在任何考试时间窗内（考试可能已结束）")]
    NotInWindow { now: String },
    #[error("包内没有任何场次")]
    NoSessions,
    #[error("场次 {id} 不存在")]
    UnknownSession { id: u32 },
    #[error("场次 {id} 不在当前时间窗内（该场时间为 {start} ~ {end}）")]
    SessionOutOfWindow { id: u32, start: String, end: String },
}

/// 一个可参与门禁判定的场次视图。
#[derive(Debug, Clone)]
pub struct SessionWindow {
    pub session_id: u32,
    pub start: u64,
    pub end: u64,
}

/// 门禁判定结果。
#[derive(Debug, Clone)]
pub struct GateDecision {
    /// 命中的场次编号
    pub session_id: u32,
    /// 当前网络时间
    pub now: u64,
    /// 是否因为时间窗重叠而自动取了一场
    pub overlapped: bool,
}

/// **门禁核心**：给定网络时间和候选场次，决定能进哪一场。
///
/// 安全约束（按重要性排序）：
///
/// 1. **时间窗是硬约束**：`explicit` 指定的场次如果不在当前时间窗内，
///    一律拒绝。这是 P0-A 的修复点——旧实现里 `explicit` 会直接返回，
///    完全跳过时间检查。
/// 2. 未指定场次时，只在时间窗内的场次里选；多个重叠时取 session_id 最小。
/// 3. 拒绝原因必须可区分（未开始 / 已结束 / 场次不存在 / 不在窗口）。
///
/// `now` 必须是**网络时间**，调用方不得用本机时钟。
pub fn decide_session(
    sessions: &[SessionWindow],
    explicit: Option<u32>,
    now: u64,
) -> Result<GateDecision, GateError> {
    if sessions.is_empty() {
        return Err(GateError::NoSessions);
    }

    // 先算时间窗内的场次——这是所有分支的共同前提。
    let in_window: Vec<&SessionWindow> = sessions
        .iter()
        .filter(|s| now >= s.start && now < s.end)
        .collect();

    // 指定了场次：必须在窗口内，否则拒绝。
    if let Some(id) = explicit {
        let target = sessions
            .iter()
            .find(|s| s.session_id == id)
            .ok_or(GateError::UnknownSession { id })?;
        if now < target.start || now >= target.end {
            return Err(GateError::SessionOutOfWindow {
                id,
                start: format_unix_local_zoned(target.start),
                end: format_unix_local_zoned(target.end),
            });
        }
        return Ok(GateDecision {
            session_id: id,
            now,
            overlapped: in_window.len() > 1,
        });
    }

    // 未指定：从窗口内选。
    match in_window.len() {
        0 => {
            let first = sessions.iter().map(|s| s.start).min().unwrap_or(0);
            if now < first {
                Err(GateError::NotStarted {
                    now: format_unix_local_zoned(now),
                    first: format_unix_local_zoned(first),
                })
            } else {
                Err(GateError::NotInWindow {
                    now: format_unix_local_zoned(now),
                })
            }
        }
        _ => {
            let chosen = in_window.iter().min_by_key(|s| s.session_id).expect("非空");
            Ok(GateDecision {
                session_id: chosen.session_id,
                now,
                overlapped: in_window.len() > 1,
            })
        }
    }
}

/// 解析 RFC3339 时间（供构建期校验与门禁用）。
///
/// 支持 `2026-12-20T09:00:00+08:00`、`...Z`、`2026-12-20 09:00:00+08:00`。
pub fn parse_rfc3339(s: &str) -> Option<u64> {
    let s = s.trim();
    let (date_part, rest) = s.split_once('T').or_else(|| s.split_once(' '))?;
    let d: Vec<&str> = date_part.split('-').collect();
    if d.len() != 3 {
        return None;
    }
    let (y, mo, da): (i64, i64, i64) = (d[0].parse().ok()?, d[1].parse().ok()?, d[2].parse().ok()?);
    // 与 Python datetime 对齐，避免 days_from_civil 的 i64 乘法溢出。
    if !(1..=9999).contains(&y) || !(1..=12).contains(&mo) || !(1..=31).contains(&da) {
        return None;
    }
    // 日历合法性：只查 1..=31 会把 2026-02-31 静默算成 3 月 3 日。
    // 两端必须同时拦（Python 侧用 datetime.date 校验）。
    if da > days_in_month(y, mo) {
        return None;
    }

    let (time_part, offset_secs) = if let Some(t) = rest.strip_suffix(['Z', 'z']) {
        (t, 0i64)
    } else if let Some(idx) = rest.rfind(['+', '-']) {
        let (t, off) = rest.split_at(idx);
        let sign = if off.starts_with('-') { -1 } else { 1 };
        let o = &off[1..];
        // 同时接受 +08:00 与 +0800；先做 ASCII 检查，避免全角字符切片 panic。
        let (oh, om) = if let Some((h, m)) = o.split_once(':') {
            (h, m)
        } else if o.len() == 4 && o.is_ascii() {
            (&o[..2], &o[2..])
        } else {
            return None;
        };
        if oh.len() != 2 || om.len() != 2 || !oh.is_ascii() || !om.is_ascii() {
            return None;
        }
        let (oh, om): (i64, i64) = (oh.parse().ok()?, om.parse().ok()?);
        // RFC3339 最大合法偏移是 +14:00。
        if !(0..=14).contains(&oh) || !(0..=59).contains(&om) || (oh == 14 && om != 0) {
            return None;
        }
        let secs = sign * (oh * 3600 + om * 60);
        (t, secs)
    } else {
        (rest, 0)
    };

    let t: Vec<&str> = time_part.split(':').collect();
    if t.len() < 2 {
        return None;
    }
    let h: i64 = t[0].parse().ok()?;
    let mi: i64 = t[1].parse().ok()?;
    let se: i64 = match t.get(2) {
        Some(x) => x.split('.').next().unwrap_or("0").parse().ok()?,
        None => 0,
    };
    if !(0..=23).contains(&h) || !(0..=59).contains(&mi) || !(0..=60).contains(&se) {
        return None;
    }

    let days = days_from_civil(y, mo, da) as i64;
    let local = days
        .checked_mul(86400)?
        .checked_add(h.checked_mul(3600)?)?
        .checked_add(mi.checked_mul(60)?)?
        .checked_add(se)?;
    let utc = local.checked_sub(offset_secs)?;
    if utc < 0 { None } else { Some(utc as u64) }
}

#[cfg(test)]
mod gate_tests {
    use super::*;

    fn win(id: u32, start: u64, end: u64) -> SessionWindow {
        SessionWindow {
            session_id: id,
            start,
            end,
        }
    }

    // 2026-12-20T09:00:00+08:00（实测值，勿手算）
    const T0900: u64 = 1_797_728_400;
    const T1100: u64 = 1_797_735_600; // +2h
    const T1300: u64 = 1_797_742_800; // +4h
    const T1500: u64 = 1_797_750_000; // +6h

    #[test]
    fn inside_window_is_allowed() {
        let s = [win(1, T0900, T1100)];
        let d = decide_session(&s, None, T0900 + 60).unwrap();
        assert_eq!(d.session_id, 1);
    }

    /// **P0-A 回归**：显式指定场次不能绕过时间窗。
    #[test]
    fn explicit_session_cannot_bypass_window() {
        let s = [win(7, T0900, T1100)];
        // 现在远早于窗口（模拟 2026-10 考 2027-06 的试）
        let err = decide_session(&s, Some(7), T0900 - 86_400 * 200).unwrap_err();
        assert!(
            matches!(err, GateError::SessionOutOfWindow { id: 7, .. }),
            "显式指定场次时，时间窗外必须拒绝，实际: {err:?}"
        );
    }

    #[test]
    fn explicit_session_after_window_is_rejected() {
        let s = [win(7, T0900, T1100)];
        let err = decide_session(&s, Some(7), T1100 + 1).unwrap_err();
        assert!(matches!(err, GateError::SessionOutOfWindow { .. }));
    }

    #[test]
    fn explicit_unknown_session_is_rejected() {
        let s = [win(1, T0900, T1100)];
        let err = decide_session(&s, Some(99), T0900 + 60).unwrap_err();
        assert!(matches!(err, GateError::UnknownSession { id: 99 }));
    }

    #[test]
    fn explicit_session_inside_window_is_allowed() {
        let s = [win(1, T0900, T1100), win(2, T1300, T1500)];
        let d = decide_session(&s, Some(2), T1300 + 10).unwrap();
        assert_eq!(d.session_id, 2);
    }

    #[test]
    fn before_all_sessions_reports_not_started() {
        let s = [win(1, T0900, T1100)];
        let err = decide_session(&s, None, T0900 - 100).unwrap_err();
        assert!(matches!(err, GateError::NotStarted { .. }));
    }

    /// 学生看到的提示必须是本地时间并带时区。
    ///
    /// 曾经用 UTC 显示：学生本地 18:28 却看到 10:28，会以为程序算错了时间。
    /// 这里直接断言用户可见文案，避免以后又被改回 UTC。
    #[test]
    fn not_started_message_is_local_and_zoned() {
        let s = [win(1, T0900, T1100)];
        let err = decide_session(&s, None, T0900 - 100).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("（UTC"), "提示缺少时区标注: {text}");
        assert!(text.contains("考试尚未开始"), "提示文案变了: {text}");
    }

    /// 窗口外解锁的提示同样要带时区。
    #[test]
    fn out_of_window_message_is_local_and_zoned() {
        let s = [win(1, T0900, T1100)];
        let err = decide_session(&s, Some(1), T1100 + 100).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("（UTC"), "提示缺少时区标注: {text}");
    }

    #[test]
    fn after_all_sessions_reports_not_in_window() {
        let s = [win(1, T0900, T1100)];
        let err = decide_session(&s, None, T1100 + 100).unwrap_err();
        assert!(matches!(err, GateError::NotInWindow { .. }));
    }

    #[test]
    fn empty_sessions_is_error() {
        assert_eq!(
            decide_session(&[], None, 0).unwrap_err(),
            GateError::NoSessions
        );
    }

    #[test]
    fn overlapping_windows_pick_smallest_id_and_flag() {
        let s = [win(5, T0900, T1100), win(3, T0900, T1100)];
        let d = decide_session(&s, None, T0900 + 10).unwrap();
        assert_eq!(d.session_id, 3);
        assert!(d.overlapped, "重叠时应收起提示");
    }

    #[test]
    fn boundary_start_inclusive_end_exclusive() {
        let s = [win(1, T0900, T1100)];
        assert!(decide_session(&s, None, T0900).is_ok(), "开始瞬间应放行");
        assert!(decide_session(&s, None, T1100).is_err(), "结束瞬间应拒绝");
        assert!(
            decide_session(&s, None, T0900 - 1).is_err(),
            "开始前 1 秒应拒绝"
        );
        assert!(
            decide_session(&s, None, T1100 + 1).is_err(),
            "结束后 1 秒应拒绝"
        );
    }

    #[test]
    fn parse_rfc3339_offsets() {
        assert_eq!(parse_rfc3339("2026-12-20T09:00:00+08:00"), Some(T0900));
        assert_eq!(parse_rfc3339("2026-12-20T09:00:00+0800"), Some(T0900));
        assert_eq!(parse_rfc3339("2026-12-20T01:00:00Z"), Some(T0900));
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("1994-11-06T08:49:37Z"), Some(784111777));
        assert!(parse_rfc3339("not-a-date").is_none());
        assert!(parse_rfc3339("2026-13-01T00:00:00Z").is_none());
        assert_eq!(parse_rfc3339("2026-12-20T09:00:00+0800"), Some(T0900));
        assert!(parse_rfc3339("2026-12-20T09:00:00+８0").is_none());
        assert!(parse_rfc3339("2026-12-20T09:00:00+14:59").is_none());
        assert!(parse_rfc3339("2026-12-20T09:00:00-1:00").is_none());
        assert!(parse_rfc3339("99999-01-01T00:00:00Z").is_none());
    }
}

#[cfg(test)]
mod signed_token_tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn config(key: &SigningKey) -> TimeConfig {
        TimeConfig {
            mode: "require".into(),
            url: "https://time.example.com/time".into(),
            pubkey: base64::engine::general_purpose::STANDARD
                .encode(key.verifying_key().to_bytes()),
            max_skew: 300,
        }
    }

    fn body(key: &SigningKey, t: u64, nonce: &str) -> String {
        let sig = key.sign(format!("{t}:{nonce}").as_bytes());
        format!(
            r#"{{"t":{t},"n":"{nonce}","sig":"{}"}}"#,
            base64::engine::general_purpose::STANDARD.encode(sig.to_bytes())
        )
    }

    #[test]
    fn accepts_valid_signed_token() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let nonce = "abc123";
        assert_eq!(
            verify_signed_token(&config(&key), &body(&key, 1_760_000_000, nonce), nonce).unwrap(),
            1_760_000_000
        );
    }

    #[test]
    fn rejects_wrong_nonce_and_tamper() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let nonce = "abc123";
        assert!(
            verify_signed_token(&config(&key), &body(&key, 1_760_000_000, "other"), nonce).is_err()
        );
        let mut text = body(&key, 1_760_000_000, nonce);
        text = text.replace("1760000000", "1760000001");
        assert!(verify_signed_token(&config(&key), &text, nonce).is_err());
    }

    #[test]
    fn nonce_is_32_hex() {
        let n = make_nonce();
        assert_eq!(n.len(), 32);
        assert!(n.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn rejects_malformed_json() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        assert!(verify_signed_token(&config(&key), "not json", "abc").is_err());
    }

    #[test]
    fn rejects_wrong_key() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let other = SigningKey::from_bytes(&[8u8; 32]);
        let nonce = "abc123";
        assert!(
            verify_signed_token(&config(&other), &body(&key, 1_760_000_000, nonce), nonce).is_err()
        );
    }
}
