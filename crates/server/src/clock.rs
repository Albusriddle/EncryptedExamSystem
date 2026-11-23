//! 服务端启动时的网络时间校验与时钟校准。
//!
//! ## 为什么服务端也要校验时间
//!
//! 场次时间窗（`start` / `end`）是硬约束。教师机的系统时钟可能被人为改过、
//! 时区设错、主板电池没电，或者长期没校时漂了几分钟。服务端若直接拿本机
//! 时钟判定"现在能不能登录"，这些偏差会原样变成业务判断：整场进不去，
//! 或者考试还没开始就放行。
//!
//! ## 为什么取时失败只告警（`warn` 是默认档）
//!
//! 考场在机房、隔离局域网里没有外网是**常见情况**，不是配置错误。把"没有
//! 外网"判成"不能开考"，造成的停考事故远大于本机时钟偏差本身。所以默认只
//! 把这件事写进启动日志（`warn`），由监考老师决定要不要处理；确实有外网、
//! 又要求强一致时间的考场显式选 `require`，那时才拒绝启动。
//!
//! ## 为什么用"偏移"而不是"取不到时间就拒绝"
//!
//! 取时只需要启动时通一次。把"网络 - 本机"的差值存进 [`Clock::offset_secs`]，
//! 之后所有场次判定都用 `本机时钟 + 偏移`：
//!
//! - 开考后网络断了不影响判定（判定不再依赖网络）；
//! - 判定口径是网络时间，不是教师机时钟——中途有人改系统时间、或者换台机器
//!   重开服务，都不会让已经开始的场次判错。
//!
//! **符号**：[`NetworkTime::local_skew_secs`] 的定义是"本机 - 网络"（本机快
//! 为正），所以要把本机时间校准到网络时间，偏移必须**取反**。见 [`evaluate`]。
//!
//! ## 纯函数边界
//!
//! [`evaluate`] 不碰网络、不读时钟，可以离线单测；[`run`] / [`run_with_sources`]
//! 只负责"取时 + 交给 evaluate"。[`TimeMode::Off`] 在两层都提前返回，保证
//! 关掉之后**一次网络请求都不会发**。

use std::time::{SystemTime, UNIX_EPOCH};

use common::timegate::{self, FetchPolicy, NetworkTime, TimeSource};

/// 默认容差：120 秒。
///
/// 正常校时的机器误差在秒级；超过 2 分钟通常意味着时区/电池/人为修改，
/// 值得提醒，但又不到"一定不能考试"的程度。
pub const DEFAULT_TOLERANCE_SECS: i64 = 120;

/// 容差下限。低于 30 秒会被正常的网络抖动与秒级取整误伤（每次取时本身就有
/// 1 秒级误差，三源之间也可能差 1 秒）。
const MIN_TOLERANCE_SECS: i64 = 30;

/// 容差上限。时钟差了一天以上，说明问题不在"漂移"，此时放宽容差毫无意义；
/// 夹住也避免调用方传入荒唐值后让判定形同儿戏。
const MAX_TOLERANCE_SECS: i64 = 86_400;

/// 启动时的时间校验档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[value(rename_all = "lower")]
pub enum TimeMode {
    /// 不校验（不会发出任何网络请求）。
    Off,
    /// 校验并告警，但不阻止启动（默认）。
    Warn,
    /// 校验，不通过就拒绝启动。
    Require,
}

impl TimeMode {
    /// 档位的稳定名字：写日志、拼拒绝原因都用它，避免 Debug 输出被改。
    pub fn as_str(self) -> &'static str {
        match self {
            TimeMode::Off => "off",
            TimeMode::Warn => "warn",
            TimeMode::Require => "require",
        }
    }
}

/// 场次判定用的时钟：本机时钟 + 网络校准偏移。
///
/// 只存偏移、不存绝对时间：绝对时间会过期，偏移在同一个进程生命周期内一直有效。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    offset_secs: i64,
}

impl Clock {
    /// 不校准，直接用本机时钟（`off` 档、或取时失败时的兜底）。
    pub fn system() -> Self {
        Self { offset_secs: 0 }
    }

    /// 带偏移的时钟。`offset_secs` 是"加在本机时间上的修正量"，不是偏差本身。
    pub fn with_offset_secs(offset_secs: i64) -> Self {
        Self { offset_secs }
    }

    pub fn offset_secs(self) -> i64 {
        self.offset_secs
    }

    /// 校准后的"现在"（Unix 秒）。
    pub fn now(self) -> u64 {
        self.now_at(local_unix())
    }

    /// 把给定的本机时间戳校准成"现在"。抽出来是为了让无网络、无等待的单测
    /// 能确定性地验证偏移，而不必等真实时间流逝。
    pub fn now_at(self, local_unix: u64) -> u64 {
        // 饱和到 0：偏移为负且本机时间很小时（1970 附近），
        // 直接相加会得到负数，转 u64 会变成一个天文数字。
        (local_unix as i64)
            .saturating_add(self.offset_secs)
            .max(0)
            .cast_unsigned()
    }
}

/// 本机原始 Unix 秒（未校准）。
///
/// `SystemTime::now()` 在系统时间早于 1970 时会返回 Err；那种情况下按 0 处理，
/// 不做 `unwrap`（契约要求任何路径都不得 panic）。
fn local_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 一次时间校验的结论。
#[derive(Debug, Clone)]
pub enum TimeReport {
    /// `off` 档：没有校验。
    Disabled,
    /// 取时成功，偏差在容差内。
    Synced { net: NetworkTime },
    /// 取时成功，但偏差超出容差。
    Skewed {
        net: NetworkTime,
        /// 实际生效的容差（已夹取到 `[30, 86400]`），拒绝原因里要写出来。
        tolerance_secs: i64,
    },
    /// 取时失败（全部失败 / 成功源不足 / 各源分歧过大都算）。
    Unavailable { error: String },
}

/// 校验结果 + 该结果对应的时钟与是否必须拒绝启动。
#[derive(Debug, Clone)]
pub struct CheckOutcome {
    pub mode: TimeMode,
    pub report: TimeReport,
    pub clock: Clock,
    /// `Some(原因)` = 必须拒绝启动；只有 `require` 档会给出。
    pub abort: Option<String>,
}

/// `off` 档的统一结论：不校验、不读时钟、不拒绝。
fn disabled(mode: TimeMode) -> CheckOutcome {
    CheckOutcome {
        mode,
        report: TimeReport::Disabled,
        // `Clock::system()` 只是"偏移 = 0"，并不读时钟；真正取时间发生在 `now()`。
        clock: Clock::system(),
        abort: None,
    }
}

/// 容差夹取。
fn clamp_tolerance(tolerance_secs: i64) -> i64 {
    tolerance_secs.clamp(MIN_TOLERANCE_SECS, MAX_TOLERANCE_SECS)
}

/// **纯函数**：把一次取时结果翻译成校验结论。
///
/// 不碰网络、不读时钟，所以失败分支、边界值都能离线单测。
///
/// 三档语义：
/// - `off`：连 `result` 都不看，直接返回 `Disabled`（"关掉就是彻底关掉"）；
/// - `warn`：成功→`Synced`/`Skewed`，失败→`Unavailable`，**永不拒绝启动**；
/// - `require`：只有 `Synced` 放行，其余都给出 `abort` 原因。
pub fn evaluate(
    mode: TimeMode,
    tolerance_secs: i64,
    result: Result<NetworkTime, String>,
) -> CheckOutcome {
    if mode == TimeMode::Off {
        return disabled(mode);
    }
    let tolerance_secs = clamp_tolerance(tolerance_secs);
    match result {
        Ok(net) => {
            let skew = net.local_skew_secs;
            // 取反的理由见模块注释：local_skew 是"本机 - 网络"，而这是"加在本机
            // 时间上的修正量"。用 saturating_neg 是为了极端值也不 panic。
            let clock = Clock::with_offset_secs(skew.saturating_neg());
            if skew.unsigned_abs() <= tolerance_secs as u64 {
                CheckOutcome {
                    mode,
                    report: TimeReport::Synced { net },
                    clock,
                    abort: None,
                }
            } else {
                // 超差时**仍然**用网络时间校准：判定口径不因为"偏差大"就退回
                // 不可信的本机时钟，这也是 warn 档只告警的底气。
                let abort = (mode == TimeMode::Require).then(|| {
                    format!("本机时钟与网络时间相差 {skew:+} 秒，超出容差 {tolerance_secs} 秒")
                });
                CheckOutcome {
                    mode,
                    report: TimeReport::Skewed {
                        net,
                        tolerance_secs,
                    },
                    clock,
                    abort,
                }
            }
        }
        Err(error) => {
            let abort =
                (mode == TimeMode::Require).then(|| format!("无法获取可信的网络时间：{error}"));
            CheckOutcome {
                mode,
                report: TimeReport::Unavailable { error },
                // 取时失败时不能凭空造偏移：offset = 0，判定与本机时钟一致。
                clock: Clock::system(),
                abort,
            }
        }
    }
}

/// 用默认三源做一次真实取时并校验（启动时调用这一条）。
pub fn run(mode: TimeMode, tolerance_secs: i64) -> CheckOutcome {
    // `off` 档连源都不构造：构造本身没有 I/O，但"关掉就是彻底关掉"更不容易出错。
    if mode == TimeMode::Off {
        return disabled(mode);
    }
    let sources = timegate::default_sources();
    run_with_sources(mode, tolerance_secs, &sources)
}

/// 可注入时间源的同款流程，供单测与需要自定义源的调用方使用。
pub fn run_with_sources(
    mode: TimeMode,
    tolerance_secs: i64,
    sources: &[Box<dyn TimeSource>],
) -> CheckOutcome {
    if mode == TimeMode::Off {
        return disabled(mode);
    }
    let policy = FetchPolicy {
        // 两个源才能交叉验证；三源时中位数能吃掉一个坏源。
        min_sources: 2,
        // 与时间门禁同一口径（见 `common::timegate::spread_limit`）。
        max_spread: 60,
        // 启动时最多重试一次：多等一次超时（约 3 秒）换"网络抖一下也能开考"。
        rounds: 2,
    };
    let result =
        timegate::fetch_network_time_with_policy(sources, policy).map_err(|e| e.to_string());
    evaluate(mode, tolerance_secs, result)
}

/// 启动日志用的多行中文说明。
///
/// 只输出"人需要的信息"：本地时间、网络时间、偏差、参与源数与最大分歧、
/// 实际生效的偏移，以及这个档位最终做了什么决定。时间格式统一交给
/// [`timegate::format_unix_local_zoned`]——本机时区与 UTC 标注必须一致，
/// 自己拼格式化很容易又写回裸 UTC（学生/教师会以为程序算错了 8 小时）。
pub fn describe(outcome: &CheckOutcome) -> String {
    let local_now = local_unix();
    let mut out = String::new();
    out.push_str(&format!(
        "时间校验：{}（{}）\n",
        outcome.mode.as_str(),
        match outcome.mode {
            TimeMode::Off => "不校验网络时间",
            TimeMode::Warn => "默认：只告警，不拒绝启动",
            TimeMode::Require => "校验不通过则拒绝启动",
        }
    ));
    out.push_str(&format!(
        "本地时间：{}\n",
        timegate::format_unix_local_zoned(local_now)
    ));

    match &outcome.report {
        TimeReport::Disabled => {
            out.push_str("生效偏移：0 秒\n");
            out.push_str("结论：未校验网络时间，完全依赖本机时钟");
            return out;
        }
        TimeReport::Unavailable { error } => {
            out.push_str(&format!("网络时间：取时失败（{error}）\n"));
            out.push_str(&format!(
                "生效偏移：{} 秒（取时失败，按本机时钟继续）\n",
                signed_secs(outcome.clock.offset_secs())
            ));
            match &outcome.abort {
                Some(reason) => out.push_str(&format!("结论：拒绝启动（{reason}）")),
                None => out.push_str(
                    "结论：仅告警（取时失败不阻止启动；机房/隔离局域网没有外网属常见情况）",
                ),
            }
        }
        TimeReport::Synced { net } => {
            out.push_str(&net_lines(net, outcome.clock, local_now));
            out.push_str(&format!(
                "结论：正常（偏差 {} 秒未超出容差）",
                signed_secs(net.local_skew_secs)
            ));
        }
        TimeReport::Skewed {
            net,
            tolerance_secs,
        } => {
            out.push_str(&net_lines(net, outcome.clock, local_now));
            // 同一个 `Skewed`，`warn` 档是告警、`require` 档是拒绝启动：结论必须
            // 由 `abort` 决定，不能按 report 写死（写死过一次，require 档的日志
            // 会显示"仅告警+校准"，值班老师看不出服务根本没起来）。
            match &outcome.abort {
                Some(reason) => out.push_str(&format!("结论：拒绝启动（{reason}）")),
                None => out.push_str(&format!(
                    "结论：仅告警（偏差 {} 秒超出容差 {tolerance_secs} 秒；\
                     场次判定已按网络时间校准）",
                    signed_secs(net.local_skew_secs)
                )),
            }
        }
    }
    out
}

/// 带符号的秒数。0 不写符号：`+0 秒`读起来像"还有一点偏差"，容易被当成问题。
fn signed_secs(secs: i64) -> String {
    if secs == 0 {
        "0".to_string()
    } else {
        format!("{secs:+}")
    }
}

/// 描述成功取时的公共几行（`Synced` / `Skewed` 共用）。
fn net_lines(net: &NetworkTime, clock: Clock, local_now: u64) -> String {
    format!(
        "网络时间：{}\n\
         偏差（本机时钟 - 网络时间）：{} 秒\n\
         参与源数：{}，最大分歧：{} 秒\n\
         生效偏移：{} 秒；校准后的现在 {}\n",
        timegate::format_unix_local_zoned(net.unix),
        signed_secs(net.local_skew_secs),
        net.sources,
        net.spread,
        signed_secs(clock.offset_secs()),
        timegate::format_unix_local_zoned(clock.now_at(local_now)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 成功返回固定时间戳的假源。
    struct FixedSource {
        name: String,
        unix: u64,
    }

    impl TimeSource for FixedSource {
        fn now_unix(&self) -> Result<u64, String> {
            Ok(self.unix)
        }
        fn name(&self) -> &str {
            &self.name
        }
    }

    /// 固定失败的假源。
    struct FailingSource {
        name: String,
        error: String,
    }

    impl TimeSource for FailingSource {
        fn now_unix(&self) -> Result<u64, String> {
            Err(self.error.clone())
        }
        fn name(&self) -> &str {
            &self.name
        }
    }

    /// 被调用就 panic 的假源，用来证明 `off` 档没有碰网络。
    ///
    /// 注意：`sample_parallel` 会给每个源开线程并 `join`，线程里的 panic 会被
    /// 吞成"该源失败"，测试**不会**因此变红。所以这里同时记调用次数，断言
    /// 计数为 0 才是真证据。
    struct PanicSource {
        calls: Arc<AtomicUsize>,
    }

    impl TimeSource for PanicSource {
        fn now_unix(&self) -> Result<u64, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            panic!("off 档不得调用任何时间源");
        }
        fn name(&self) -> &str {
            "panic"
        }
    }

    fn fixed(name: &str, unix: u64) -> Box<dyn TimeSource> {
        Box::new(FixedSource {
            name: name.into(),
            unix,
        })
    }

    fn failing(name: &str, error: &str) -> Box<dyn TimeSource> {
        Box::new(FailingSource {
            name: name.into(),
            error: error.into(),
        })
    }

    /// 构造一份取时结果。`local_skew_secs` 口径与 `timegate` 一致：
    /// 正值 = 本机时钟比网络时间快。
    fn net(sources: usize, spread: u64, local_skew_secs: i64) -> NetworkTime {
        NetworkTime {
            unix: 1_700_000_000,
            sources,
            spread,
            local_skew_secs,
        }
    }

    /// `off` 档必须一次源都不调用，且结论是 Disabled、偏移 0、不拒绝启动。
    #[test]
    fn off_mode_never_touches_any_source() {
        let calls = Arc::new(AtomicUsize::new(0));
        let sources: Vec<Box<dyn TimeSource>> = (0..3)
            .map(|_| {
                Box::new(PanicSource {
                    calls: Arc::clone(&calls),
                }) as Box<dyn TimeSource>
            })
            .collect();

        let outcome = run_with_sources(TimeMode::Off, DEFAULT_TOLERANCE_SECS, &sources);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "off 档不得调用任何时间源");
        assert!(matches!(outcome.report, TimeReport::Disabled));
        assert_eq!(outcome.mode, TimeMode::Off);
        assert_eq!(outcome.clock.offset_secs(), 0);
        assert!(outcome.abort.is_none());

        // evaluate 同样是纯的：即使塞进一个"成功取时"，off 档也一律忽略。
        let pure = evaluate(TimeMode::Off, DEFAULT_TOLERANCE_SECS, Ok(net(3, 0, -500)));
        assert!(
            matches!(pure.report, TimeReport::Disabled),
            "off 档必须忽略取时结果"
        );
        assert_eq!(pure.clock.offset_secs(), 0, "off 档不得带入任何偏移");
        assert!(pure.abort.is_none());
    }

    /// 三源一致 → Synced，且校准后的"现在"就是网络时间（不是本机时间）。
    ///
    /// 符号：`local_skew_secs` 是「本机 - 网络」。本机慢 100 秒时 skew = -100，
    /// 生效偏移必须是 +100，`now_at(1_000_000)` 才等于 1_000_100（网络时间）。
    #[test]
    fn agreeing_sources_sync_clock_to_network_time() {
        let outcome = evaluate(TimeMode::Warn, DEFAULT_TOLERANCE_SECS, Ok(net(3, 0, -100)));
        match &outcome.report {
            TimeReport::Synced { net } => {
                assert_eq!(net.sources, 3);
                assert_eq!(net.spread, 0);
            }
            other => panic!("三源一致且偏差在容差内应为 Synced，实际 {other:?}"),
        }
        assert!(outcome.abort.is_none());
        assert_eq!(
            outcome.clock.offset_secs(),
            100,
            "偏移要把本机时间校准到网络时间"
        );
        assert_eq!(
            outcome.clock.now_at(1_000_000),
            1_000_100,
            "校准后必须等于网络时间：now_at(local) = local + 生效偏移"
        );

        // 真实取时链路（假源、不联网）也要能走到 Synced，且校准后的"现在"
        // 落在网络时间上（允许 2 秒的取整/流逝误差）。
        let local_now = Clock::system().now();
        let net_unix = local_now.saturating_sub(100); // 本机快 100 秒
        let sources = vec![
            fixed("baidu", net_unix),
            fixed("aliyun", net_unix),
            fixed("tencent", net_unix),
        ];
        let live = run_with_sources(TimeMode::Warn, DEFAULT_TOLERANCE_SECS, &sources);
        match &live.report {
            TimeReport::Synced { net } => {
                assert_eq!(net.sources, 3);
                assert!(
                    live.clock.now().abs_diff(net.unix) <= 2,
                    "场次判定必须跟网络时间走，实际 {live:?}"
                );
                assert_eq!(live.clock.offset_secs(), -net.local_skew_secs);
            }
            other => panic!("三源一致应为 Synced，实际 {other:?}"),
        }
    }

    /// `warn` 档：偏差超容差只告警，不拒绝启动；偏移仍然生效。
    #[test]
    fn warn_mode_warns_but_still_starts() {
        let outcome = evaluate(TimeMode::Warn, DEFAULT_TOLERANCE_SECS, Ok(net(3, 2, 600)));
        match &outcome.report {
            TimeReport::Skewed {
                net,
                tolerance_secs,
            } => {
                assert_eq!(*tolerance_secs, 120);
                assert_eq!(net.local_skew_secs, 600);
            }
            other => panic!("偏差 600 秒远超容差，应为 Skewed，实际 {other:?}"),
        }
        assert!(outcome.abort.is_none(), "warn 档不得拒绝启动");
        assert_eq!(
            outcome.clock.offset_secs(),
            -600,
            "超差也要按网络时间校准，不能退回本机时钟"
        );
        // 本机时间 = 网络时间 + 600；校准后应回到网络时间。
        assert_eq!(
            outcome.clock.now_at(1_700_000_600),
            1_700_000_000,
            "场次判定应跟网络时间走"
        );
    }

    /// `require` 档：超差必须拒绝启动，且原因里写明偏差与容差的具体数值。
    #[test]
    fn require_mode_refuses_on_skew_with_numbers() {
        let outcome = evaluate(
            TimeMode::Require,
            DEFAULT_TOLERANCE_SECS,
            Ok(net(3, 0, 600)),
        );
        assert!(matches!(outcome.report, TimeReport::Skewed { .. }));
        let reason = outcome
            .abort
            .as_deref()
            .expect("require 档超差必须给出拒绝原因");
        assert!(reason.contains("600"), "拒绝原因要写出实际偏差: {reason}");
        assert!(reason.contains("120"), "拒绝原因要写出容差: {reason}");
        assert_eq!(outcome.mode, TimeMode::Require);
    }

    /// `warn` 档：三种取时失败都只告警，且绝不造偏移。
    #[test]
    fn warn_mode_treats_every_fetch_failure_as_unavailable() {
        let cases: Vec<(&str, Vec<Box<dyn TimeSource>>)> = vec![
            (
                "全部失败",
                vec![failing("a", "超时"), failing("b", "拒绝连接")],
            ),
            (
                "成功源不足",
                vec![
                    fixed("a", 1_700_000_000),
                    failing("b", "超时"),
                    failing("c", "超时"),
                ],
            ),
            (
                "各源分歧过大",
                vec![
                    fixed("a", 1_700_000_000),
                    fixed("b", 1_700_000_100),
                    fixed("c", 1_700_000_000),
                ],
            ),
        ];
        for (label, sources) in cases {
            let outcome = run_with_sources(TimeMode::Warn, DEFAULT_TOLERANCE_SECS, &sources);
            assert!(
                matches!(outcome.report, TimeReport::Unavailable { .. }),
                "{label} 应判为 Unavailable，实际 {:?}",
                outcome.report
            );
            assert!(outcome.abort.is_none(), "{label}: warn 档不得拒绝启动");
            assert_eq!(
                outcome.clock.offset_secs(),
                0,
                "{label}: 取时失败不能凭空加偏移"
            );
            let text = describe(&outcome);
            assert!(
                text.contains("隔离局域网"),
                "{label}: 提示要说明没有外网属常见情况:\n{text}"
            );
        }
    }

    /// `require` 档：取时失败必须拒绝启动，原因里带失败摘要。
    #[test]
    fn require_mode_refuses_when_fetch_fails() {
        let sources = vec![
            failing("baidu", "请求失败: 连接超时"),
            failing("aliyun", "请求失败: 连接超时"),
        ];
        let outcome = run_with_sources(TimeMode::Require, DEFAULT_TOLERANCE_SECS, &sources);
        assert!(matches!(outcome.report, TimeReport::Unavailable { .. }));
        let reason = outcome
            .abort
            .as_deref()
            .expect("require 档取时失败必须拒绝启动");
        assert!(
            reason.contains("baidu") || reason.contains("时间源"),
            "拒绝原因要带失败摘要: {reason}"
        );
        assert_eq!(outcome.clock.offset_secs(), 0);
        let text = describe(&outcome);
        assert!(text.contains("拒绝启动"), "启动日志要写明拒绝启动:\n{text}");
    }

    /// 边界：`|skew| == tolerance` 视为正常（含负偏差）；多 1 秒才算超差。
    #[test]
    fn skew_exactly_at_tolerance_is_synced() {
        for skew in [120i64, -120] {
            let outcome = evaluate(TimeMode::Warn, DEFAULT_TOLERANCE_SECS, Ok(net(3, 1, skew)));
            assert!(
                matches!(outcome.report, TimeReport::Synced { .. }),
                "偏差 {skew:+} 秒恰好等于容差，应视为正常，实际 {:?}",
                outcome.report
            );
            assert!(outcome.abort.is_none());
        }
        let over = evaluate(
            TimeMode::Require,
            DEFAULT_TOLERANCE_SECS,
            Ok(net(3, 1, -121)),
        );
        assert!(
            matches!(over.report, TimeReport::Skewed { .. }),
            "负偏差 -121 秒超出 120 秒容差应为 Skewed"
        );
        assert!(over.abort.is_some(), "require 档负偏差超差也要拒绝");
    }

    /// 校准结果早于 1970 时必须饱和为 0，不能回绕成天文数字。
    #[test]
    fn now_at_saturates_at_zero() {
        let clock = Clock::with_offset_secs(-5_000);
        assert_eq!(clock.now_at(0), 0);
        assert_eq!(clock.now_at(5_000), 0);
        assert_eq!(clock.now_at(5_001), 1, "恰好越过 0 时只多 1 秒");
        // 极端偏移也不能 panic（取反/相加都走饱和路径）。
        assert_eq!(Clock::with_offset_secs(i64::MIN).now_at(0), 0);
        assert_eq!(Clock::with_offset_secs(i64::MAX).now_at(0), i64::MAX as u64);
    }

    /// 容差夹取到 `[30, 86400]`：传 5 实际按 30 判，传天文数字按 86400 判。
    ///
    /// 注意"按 30 判"的后果：`skew = 20` 在容差 5 下本该超差，夹到 30 之后
    /// 就算正常了——这是夹取下限**故意**的方向：宁可少报警，也不让正常的秒级
    /// 抖动把 `require` 档的考场拦在门外。
    #[test]
    fn tolerance_is_clamped() {
        // 传 5，实际生效 30：31 秒才超差，且报告里的容差是夹取后的值。
        let low = evaluate(TimeMode::Warn, 5, Ok(net(3, 0, 31)));
        match &low.report {
            TimeReport::Skewed { tolerance_secs, .. } => assert_eq!(*tolerance_secs, 30),
            other => panic!("容差 5 秒应被夹到 30 秒，31 秒偏差因此算超差: {other:?}"),
        }

        // 30 秒以内（含边界）视为正常。
        for skew in [30i64, -30, 20] {
            let ok = evaluate(TimeMode::Warn, 5, Ok(net(3, 0, skew)));
            assert!(
                matches!(ok.report, TimeReport::Synced { .. }),
                "夹取后的 30 秒边界内应放行，实际 skew={skew}: {:?}",
                ok.report
            );
        }

        // 上限：一天以上的偏差仍然超差，而不是"容差大到永远正常"。
        let high = evaluate(TimeMode::Warn, 1_000_000, Ok(net(3, 0, 86_401)));
        match &high.report {
            TimeReport::Skewed { tolerance_secs, .. } => assert_eq!(*tolerance_secs, 86_400),
            other => panic!("容差应被夹到 86400 秒: {other:?}"),
        }
    }

    /// 启动日志必须包含各自的关键数字，`off` 档要说明完全依赖本机时钟。
    #[test]
    fn describe_reports_numbers_and_conclusion() {
        let outcome = evaluate(
            TimeMode::Require,
            DEFAULT_TOLERANCE_SECS,
            Ok(net(3, 4, 600)),
        );
        let text = describe(&outcome);
        for want in [
            "本地时间",
            "网络时间",
            "+600",
            "120",
            "参与源数：3",
            "最大分歧：4 秒",
            "生效偏移",
            "结论：拒绝启动（",
            "（UTC",
        ] {
            assert!(text.contains(want), "启动日志缺少「{want}」:\n{text}");
        }
        // 拒绝原因必须真的写进结论，而不是只靠档位说明里的"拒绝启动"四个字蒙混。
        assert!(
            text.contains("相差 +600 秒"),
            "拒绝原因要带具体偏差数值:\n{text}"
        );

        let off = describe(&evaluate(
            TimeMode::Off,
            DEFAULT_TOLERANCE_SECS,
            Err(String::new()),
        ));
        assert!(
            off.contains("完全依赖本机时钟"),
            "off 档要说明完全依赖本机时钟:\n{off}"
        );
        assert!(off.contains("off"), "日志要写明当前档位:\n{off}");

        // 同样的 Skewed，warn 档必须写成"仅告警"，不能也是"拒绝启动"。
        let warn = describe(&evaluate(
            TimeMode::Warn,
            DEFAULT_TOLERANCE_SECS,
            Ok(net(3, 1, -300)),
        ));
        assert!(
            warn.contains("结论：仅告警（"),
            "warn 档超差只应告警:\n{warn}"
        );
        assert!(
            !warn.contains("结论：拒绝启动"),
            "warn 档不得写成拒绝启动:\n{warn}"
        );
        // 正常分支同样要能落到"正常"。
        let synced = describe(&evaluate(
            TimeMode::Warn,
            DEFAULT_TOLERANCE_SECS,
            Ok(net(3, 0, 0)),
        ));
        assert!(
            synced.contains("结论：正常（"),
            "偏差在容差内应为正常:\n{synced}"
        );
    }
}
