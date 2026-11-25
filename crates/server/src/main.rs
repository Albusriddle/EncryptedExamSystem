//! 网页版学生端（阶段 1）。
//!
//! 设计取舍：
//! - **只从已构建的考试包启动**：读 payload + K_manifest，不持有 master、不持有答案；
//! - 登录即校验：复用 `container::try_unlock_session_with_pin`，学号/PIN 错就没有信封；
//! - 每人只生成自己的试卷（`picker`），接口不会下发“整库”；
//! - 作答先放内存，交卷时用 `answer_sheet::seal` 写出与离线版同格式的 `.eea`。
//!
//! 仍是**阶段 1 实现**，不是生产形态：没有 SQLite，管理端只有启动入口，登录限速也很粗糙。
//! v1.5.0 起默认 HTTPS（自签证书；`--tls off` 回退明文）。

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use common::{answer_sheet, container, crypto, picker};
use rand::RngCore;
use serde_json::{Value, json};

const INDEX_HTML: &str = include_str!("index.html");
/// 安全策略：默认全禁，只放行同源资源。
///
/// 安全响应头（全局）。
///
/// `script-src 'self'` 之所以能这么严：行内 `<script>`/`<style>` 已拆到 `/assets/app.{js,css}`，
/// 页面里也不再有行内 `style=` 属性（改用工具类；JS 里 `el.style.x = …` 属 CSSOM，不受 CSP 限制）。
/// 这样即使将来某个 sink 写错，也没有「内联脚本执行」这条路。
const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
/// 作答接口的请求体上限。
///
/// 「一题答案 ≤1 MiB」在 JSON 里最坏会被转义成 2 倍（`\n`/`"`/`\`），所以留 4 MiB；
/// 其余接口（登录、交卷、静态资源）只需要几十字节，64 KiB 足够。
/// 按路由限幅而不是全局：原来 20 MiB × 256 连接的最坏内存是 5 GiB。
const MAX_BODY_ANSWER: usize = 4 * 1024 * 1024;
const MAX_BODY_DEFAULT: usize = 64 * 1024;

/// 某条路径允许的请求体上限。
fn max_body_for(path: &str) -> usize {
    if path == "/api/answer" {
        MAX_BODY_ANSWER
    } else {
        MAX_BODY_DEFAULT
    }
}

mod clock;
mod secure_fs;
mod tls;

#[cfg(test)]
mod server_tests;

/// 仅测试用的「密封阶段」观察点。
///
/// 用来证明 [`seal_and_export`]（加密答题卡 + 写盘）**没有**在持有 `App::state` 锁时执行。
/// 同线程里 `try_lock` 对自己的锁必然失败，所以这个断言是确定性的，不靠计时：
/// 旧实现（锁内冻结）会让 `state.try_lock()` 返回 `Err`，新实现则成功。
///
/// 只在测试构建里存在，release 二进制里没有任何开关。
#[cfg(test)]
pub(crate) mod seal_hook {
    use std::sync::{Arc, Mutex};

    pub(crate) type Observer = Arc<dyn Fn(bool) + Send + Sync>;

    static OBSERVER: Mutex<Option<Observer>> = Mutex::new(None);

    pub(crate) fn set(observer: Option<Observer>) {
        *OBSERVER.lock().unwrap() = observer;
    }

    /// `started = true` 表示刚进入密封阶段，`false` 表示即将返回。
    pub(crate) fn notify(started: bool) {
        let observer = OBSERVER.lock().unwrap().as_ref().cloned();
        if let Some(observer) = observer {
            observer(started);
        }
    }
}

/// TLS 模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum TlsMode {
    /// 默认：有证书就用，没有就自动生成一张自签证书
    Auto,
    /// 明文 HTTP（不推荐：同网段可嗅探；只用于排障）
    Off,
}

/// 外部证书自检的严格程度。
///
/// 默认 `warn`：只有「学生根本连不上」的问题才拒绝启动；过期 / SAN 不覆盖某地址
/// 这类「还能点继续」的问题只告警——考场里把服务停掉比让学生多看一眼警告更糟。
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum TlsCheck {
    /// 默认：致命项拒绝启动，警告项只提示
    Warn,
    /// 严格：警告项也拒绝启动（适合要求「全班看到的警告必须一致」的内网）
    Strict,
}

#[derive(Parser)]
#[command(name = "exam-server", version, about = "保密考试系统网页版（阶段 1）")]
struct Cli {
    /// 已构建的考试包（exam-builder 产物；`--gen-cert` 时可省略）
    #[arg(long)]
    package: Option<PathBuf>,
    /// K_manifest（64 位 hex 文件，`exam-builder build --emit-keyfile` 生成）
    #[arg(long)]
    keyfile: Option<PathBuf>,
    /// 监听地址
    #[arg(long, default_value = "0.0.0.0")]
    bind: String,
    /// HTTPS 端口（默认 8443）
    #[arg(long, default_value_t = 8443)]
    port: u16,
    /// TLS 模式：auto（默认，没证书就自动生成自签证书）/ off（明文，不推荐）
    #[arg(long, value_enum, default_value_t = TlsMode::Auto)]
    tls: TlsMode,
    /// 已有证书（不给就自动生成到 <answers-dir>/tls/）
    #[arg(long)]
    tls_cert: Option<PathBuf>,
    /// 已有私钥
    #[arg(long)]
    tls_key: Option<PathBuf>,
    /// 证书 SAN 里要包含的地址（可多次；默认=本机局域网 IP + 127.0.0.1 + localhost；
    /// `--bind` 指定具体地址时该地址也会并入 SAN）
    #[arg(long = "tls-host")]
    tls_hosts: Vec<String>,
    /// 外部证书的签发 CA（可选；只用于启动自检的链校验，不改变发给学生的证书链）
    #[arg(long)]
    tls_ca: Option<PathBuf>,
    /// 外部证书自检严格程度：warn（默认，过期/SAN 缺失只告警）/ strict（同样拒绝启动）
    #[arg(long, value_enum, default_value_t = TlsCheck::Warn)]
    tls_check: TlsCheck,
    /// 明文 301 跳转端口（只回 301 到 https，便于旧书签；0 = 关闭）
    #[arg(long, default_value_t = 8080)]
    http_redirect_port: u16,
    /// 只生成自签证书然后退出（可配 --tls-host / --answers-dir）
    #[arg(long)]
    gen_cert: bool,
    /// 指定场次（默认选当前时间窗内的场次）
    #[arg(long)]
    session: Option<u32>,
    /// 答题卡输出目录（默认 ./answers）
    #[arg(long, default_value = "answers")]
    answers_dir: PathBuf,
    /// 作答状态目录（默认 <answers-dir>/state）
    #[arg(long)]
    state_dir: Option<PathBuf>,
    /// 启动时校验网络时间：off / warn（默认，只告警）/ require（不通过拒绝启动）
    ///
    /// 机房、隔离局域网没有外网是常态，所以默认只告警；
    /// 有外网且要求强一致时间的考场再用 require；off 会让场次判定退回教师机系统时钟（不建议）。
    #[arg(long, value_enum, default_value = "warn")]
    time_check: clock::TimeMode,
    /// 本机时钟与网络时间的允许偏差（秒），实际夹取到 30–86400
    #[arg(long, default_value_t = clock::DEFAULT_TOLERANCE_SECS)]
    time_tolerance: i64,
    /// 同一 IP 每分钟允许的登录尝试次数（0 = 不限制）
    ///
    /// 默认 60：一个考场 50 人，正常每 IP 只有一两次；留出余量给「学生在同一 NAT 后面」
    /// 或老师复测的场景。真正防 PIN 爆破的是按学号的 10 次/分钟。
    #[arg(long, default_value_t = 60)]
    login_rate_limit: u32,
}

/// 每个学生的写盘锁：`(学号, 场次)` → 锁。
///
/// 抽成别名一是为了读，二是 clippy 的 `type_complexity`：Map 里再套 `Arc<Mutex<_>>`。
type StudentWriteLocks = Mutex<HashMap<(u64, u32), Arc<Mutex<()>>>>;

struct App {
    payload: Vec<u8>,
    k_manifest: [u8; 32],
    answers_dir: PathBuf,
    state_dir: PathBuf,
    forced_session: Option<u32>,
    state_key: [u8; 32],
    /// 场次判定用的时钟：本机时钟 + 启动时的网络校准偏移。
    clock: clock::Clock,
    /// 启动时的校验档位，只用于 `/api/time` 的诊断输出。
    time_mode: clock::TimeMode,
    /// 同一 IP 每分钟的登录尝试上限（0 = 不限制）。
    login_rate_limit: u32,
    /// 事件日志的写锁。
    ///
    /// `events.jsonl` 由交卷 / 保存并发写入。多个线程各持一个 append 句柄时，一次
    /// `write` 若被拆成两次（`writeln!` 就是先写正文再写换行），两行会在字节层面
    /// 交错成无法解析的乱码——v1.4.5 的 12 人并发演练实测到：12 条交卷只解析出 2 条。
    event_log: Mutex<()>,
    /// 按学生分桶的状态写锁。
    ///
    /// 同一学生的「保存」与「交卷」必须串行，否则保存的「未交卷」快照可能后落地、
    /// 把交卷写下的「已交卷」覆盖回去。但**不该全局串行**：50 人同时交卷时，
    /// 每次 durable fsync 都会把别人的保存排在后面（v1.4.4 花力气去掉的「一个人堵全场」）。
    /// 锁序：`write_locks → 学生锁 → state`，没有反向路径。
    write_locks: StudentWriteLocks,
    /// 状态文件临时名序号：并发写各自的 `.tmp`，避免互相 rename 掉。
    tmp_seq: std::sync::atomic::AtomicU64,
    conns: std::sync::atomic::AtomicUsize,
    gate: Gate,
    state: Mutex<State>,
    /// 场次时间窗缓存（payload 在进程生命周期内不变，没必要每个请求解密 header）。
    windows: HashMap<u32, (u64, u64)>,
    /// 自动收卷失败的退避状态（学号 → (失败次数, 下次重试时刻)）。
    finalize_retry: Mutex<HashMap<u64, (u32, u64)>>,
    /// TLS 配置；`None` = 明文模式（`--tls off`）。
    ///
    /// 存在 App 里是为了让 `handle()` 在同一套「每连接一线程」模型里决定要不要握手。
    tls: Option<Arc<rustls::ServerConfig>>,
    /// cookie 是否加 `Secure`（TLS 下必须加）。
    secure_cookies: bool,
    /// 包里的「学生回看」策略。离线端一直按它控制交卷后能不能看卷；网页端 v1.4.10
    /// 及以前**完全忽略**它（交卷后照样能看全卷），与「仅教师可查看原卷」的默认相反。
    review: common::model::StudentReviewPolicy,
}

#[derive(Default)]
struct State {
    students: HashMap<String, Student>,
    /// 学号 → 当前有效 token（单会话：重新登录会顶掉旧 token）
    tokens_by_student: HashMap<u64, String>,
    attempts: HashMap<IpAddr, (u32, u64)>,
    attempts_student: HashMap<u64, (u32, u64)>,
}

struct Student {
    student_id: u64,
    session_id: u32,
    paper: picker::Paper,
    answers: BTreeMap<String, String>,
    submitted: bool,
    /// 已导出的答题卡文件名（幂等，避免重复写盘）
    exported: Option<String>,
    last_seen: u64,
}

struct Request {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
    peer: IpAddr,
}

struct Response {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
    headers: Vec<(String, String)>,
}

impl Response {
    fn json(status: u16, value: Value) -> Self {
        Self {
            status,
            content_type: "application/json; charset=utf-8",
            body: value.to_string().into_bytes(),
            headers: Vec::new(),
        }
    }
    fn error(status: u16, msg: impl Into<String>) -> Self {
        Self::json(status, json!({"error": msg.into()}))
    }
    fn asset(content_type: &'static str, body: &'static str) -> Self {
        Self {
            status: 200,
            content_type,
            body: body.as_bytes().to_vec(),
            headers: Vec::new(),
        }
    }
    fn html_owned(body: String) -> Self {
        Self {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: body.into_bytes(),
            headers: Vec::new(),
        }
    }
    fn with_cookie(mut self, cookie: String) -> Self {
        self.headers.push(("Set-Cookie".into(), cookie));
        self
    }
}

/// 会话 cookie 的有效期（秒）。滑动续期：只要还在答题就一直有效。
///
/// 为什么不是「直到考试结束」：共享设备上留一个长期可续的会话，下一个人打开浏览器
/// 就直接进了前一个学生的卷子。60 分钟纯空闲才失效，配合「提交/退出即失效」，
/// 既能让关掉浏览器的学生重开后自动回到原卷，又不会把会话留在机器上。
const SESSION_COOKIE_MAX_AGE: u64 = 3600;

/// 正常会话 cookie（60 分钟，滑动续期）。
///
/// TLS 下必须带 `Secure`：否则浏览器会允许这条 cookie 走明文通道，
/// 而我们在 HTTPS 模式下只监听 TLS 端口——明文请求根本到不了，`Secure` 只是显式声明。
fn session_cookie(app: &App, token: &str) -> String {
    let secure = if app.secure_cookies { "; Secure" } else { "" };
    format!(
        "ees_token={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={SESSION_COOKIE_MAX_AGE}{secure}"
    )
}

/// 立即失效的会话 cookie（提交、退出）。
fn expired_session_cookie(app: &App) -> String {
    let secure = if app.secure_cookies { "; Secure" } else { "" };
    format!("ees_token=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{secure}")
}

/// 交卷之后的会话 cookie。
///
/// 默认策略（「仅教师可查看原卷」）下立即失效：交完就不该再有可续的会话。
/// 允许回看时保持会话——否则学生交完卷就再也看不到自己写的（回看策略形同虚设）。
fn post_submit_cookie(app: &App, token: &str) -> String {
    if app.review == common::model::StudentReviewPolicy::Free {
        session_cookie(app, token)
    } else {
        expired_session_cookie(app)
    }
}

/// 限制同时进行的 Argon2 登录数：每人 64 MiB，150 人并发会打爆内存。
struct Gate {
    slots: Mutex<usize>,
    cv: Condvar,
    max: usize,
}

impl Gate {
    fn new(max: usize) -> Self {
        Self {
            slots: Mutex::new(0),
            cv: Condvar::new(),
            max,
        }
    }
    /// 取一个槽位；守卫 Drop 时归还。
    ///
    /// 为什么必须 RAII：手写 `release` 时，`unlock()` 里任何 panic 都会让计数永久 +1，
    /// 4 次之后所有登录都会卡在 `cv.wait` 上——不是报错，是线程永久阻塞。
    fn acquire(&self) -> GateSlot<'_> {
        let mut n = self
            .slots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while *n >= self.max {
            n = self
                .cv
                .wait(n)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        *n += 1;
        GateSlot(self)
    }
}

/// 槽位守卫：离开作用域即归还并唤醒一个等待者。
struct GateSlot<'a>(&'a Gate);

impl Drop for GateSlot<'_> {
    fn drop(&mut self) {
        let mut n = self
            .0
            .slots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *n = n.saturating_sub(1);
        self.0.cv.notify_one();
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // --gen-cert：只生成证书，不需要考试包。
    if cli.gen_cert {
        if cli.tls_ca.is_some() || cli.tls_check != TlsCheck::Warn {
            println!(
                "⚠ --tls-ca / --tls-check 只用于外部证书自检；--gen-cert 生成的是自签证书，忽略这两个参数"
            );
        }
        let hosts = tls_hosts(&cli);
        let dir = cli.answers_dir.join("tls");
        tls::generate(&dir, &hosts, 365)?;
        println!("已生成自签证书：{}", dir.display());
        println!("SAN：{}", hosts.join(", "));
        // 绑了具体地址就用它：`--bind ::1` 时学生该访问 `https://[::1]:8443/`，
        // 而不是 hosts[0] 里那个局域网 IP。
        let display_host = if matches!(cli.bind.as_str(), "0.0.0.0" | "::" | "[::]") {
            hosts[0].clone()
        } else {
            bind_host(&cli.bind)
        };
        println!(
            "学生访问：https://{}:{}/",
            url_host(&display_host),
            cli.port
        );
        return Ok(());
    }
    let (Some(package), Some(keyfile)) = (cli.package.as_ref(), cli.keyfile.as_ref()) else {
        anyhow::bail!("缺少 --package / --keyfile（或改用 --gen-cert 只生成证书）");
    };

    // 先校时、再读包、再绑端口：`require` 档不通过就拒绝启动，避免"服务起来了，
    // 但场次判定用的是不可信的教师机时钟"。`warn` 档只打印结论，不阻止启动。
    // 进度提示放在最前面：无外网时这一步要等约 3 秒，否则看起来像卡住。
    println!(
        "正在校验网络时间（{} 档；机房无外网时会等约 3 秒，然后按本机时钟继续）…",
        cli.time_check.as_str()
    );
    let check = clock::run(cli.time_check, cli.time_tolerance);
    for line in clock::describe(&check).lines() {
        println!("{line}");
    }
    if let Some(reason) = &check.abort {
        anyhow::bail!("时间校验不通过：{reason}（如确需跳过，可加 --time-check warn）");
    }
    let server_clock = check.clock;

    let bytes =
        std::fs::read(package).with_context(|| format!("读取考试包 {}", package.display()))?;
    let payload = container::extract_payload(&bytes)
        .context("考试包中没有 payload（请用 exam-builder 生成）")?;

    let key_text = std::fs::read_to_string(keyfile)
        .with_context(|| format!("读取 keyfile {}", keyfile.display()))?;
    let key_bytes = hex::decode(key_text.trim()).context("keyfile 不是合法 hex")?;
    anyhow::ensure!(key_bytes.len() == 32, "keyfile 必须是 64 位 hex（32 字节）");
    let mut k_manifest = [0u8; 32];
    k_manifest.copy_from_slice(&key_bytes);

    let (header, _) = container::parse_header(payload, &k_manifest).context("解析 header 失败")?;
    // 场次窗口只算一次：payload 在进程生命周期内不变，而原来每个请求、每个学生都要
    // 解密一次 header（50 人 × 每次保存都在锁内做一次 AES-GCM）。
    // 严格解析（必须带时区偏移、结束晚于开始）：解析失败直接拒绝启动，
    // 而不是把无偏移时间当 UTC、静默偏 8 小时。
    let windows = session_windows_from(&header).map_err(|e| anyhow::anyhow!("{e}"))?;
    // 用校准后的时间挑场次：本机时钟偏差大时，挑中哪一场会跟着变。
    // 与运行期判定同口径 `[start, end)`（原来这里用 `now <= end`，比别的路径多开一秒）。
    let now = server_clock.now();
    let active = header.sessions.iter().find(|s| {
        windows
            .get(&s.session_id)
            .is_some_and(|&(a, b)| window_open(now, a, b))
    });
    let session_id = cli
        .session
        .or_else(|| active.map(|s| s.session_id))
        .or_else(|| header.sessions.first().map(|s| s.session_id));
    let Some(session_id) = session_id else {
        anyhow::bail!("考试包里没有任何场次")
    };

    std::fs::create_dir_all(&cli.answers_dir)
        .with_context(|| format!("创建答题卡目录 {}", cli.answers_dir.display()))?;
    let state_dir = cli
        .state_dir
        .clone()
        .unwrap_or_else(|| cli.answers_dir.join("state"));
    std::fs::create_dir_all(&state_dir)
        .with_context(|| format!("创建状态目录 {}", state_dir.display()))?;
    harden_state_dir(&state_dir);

    // ---- TLS：默认开；没证书就自动生成 ----
    let hosts = tls_hosts(&cli);
    // 先把「cert/key 必须成对」判掉，再谈自检：否则只给 --tls-cert 时，
    // 会先打印「当前是自签证书，忽略这两个参数」，然后再报「必须同时给出」——顺序与文案都错。
    let cert_pair = match (&cli.tls_cert, &cli.tls_key) {
        (Some(cert), Some(key)) => Some((cert, key)),
        (None, None) => None,
        _ => anyhow::bail!("--tls-cert 与 --tls-key 必须同时给出（只给了其中一个）"),
    };

    // ---- 外部证书自检（只对外部证书跑：自签证书是本机生成的，没有「配置错」这回事） ----
    let cert_check = match (&cli.tls, cert_pair) {
        (TlsMode::Auto, Some((cert, key))) => {
            let check = tls::check_external(cert, key, cli.tls_ca.as_deref(), &hosts)?;
            // 致命项存在时**也把警告打出来**：否则老师修完致命项重启，才发现还有两条警告，
            // 白折腾一轮。
            if !check.passed() {
                for msg in &check.fatals {
                    eprintln!("✗ 外部证书自检未通过：{msg}");
                }
                if check.warnings.is_empty() {
                    anyhow::bail!(
                        "外部证书自检未通过（{} 项致命），每条的修法见上",
                        check.fatals.len()
                    );
                }
                for msg in &check.warnings {
                    eprintln!("⚠ 外部证书自检（另有警告）：{msg}");
                }
                anyhow::bail!(
                    "外部证书自检未通过（{} 项致命、{} 项警告），每条的修法见上",
                    check.fatals.len(),
                    check.warnings.len()
                );
            }
            if cli.tls_check == TlsCheck::Strict && !check.warnings.is_empty() {
                for msg in &check.warnings {
                    eprintln!("✗ 外部证书自检（strict）：{msg}");
                }
                anyhow::bail!(
                    "外部证书自检有 {} 项警告，而 --tls-check strict 要求零警告",
                    check.warnings.len()
                );
            }
            for msg in &check.warnings {
                eprintln!("⚠ 外部证书自检：{msg}");
            }
            Some(check)
        }
        (why, _) => {
            // 参数给了但当前不会自检：提示一句，避免老师以为「加了 --tls-ca 就生效了」。
            if cli.tls_ca.is_some() || cli.tls_check != TlsCheck::Warn {
                let why_text = if *why == TlsMode::Off {
                    "明文（--tls off）"
                } else {
                    "自签证书（没给 --tls-cert/--tls-key）"
                };
                println!(
                    "⚠ --tls-ca / --tls-check 仅对外部证书生效：当前是{why_text}，忽略这两个参数"
                );
            }
            None
        }
    };
    let tls_material = match cli.tls {
        TlsMode::Off => None,
        TlsMode::Auto => Some(match (&cli.tls_cert, &cli.tls_key) {
            // 老师给了自己的证书（例如用内网 CA 签的）：只加载，绝不覆盖。
            (Some(cert), Some(key)) => tls::load(cert, key)?,
            (None, None) => tls::load_or_generate(&cli.answers_dir.join("tls"), &hosts, 365)?,
            _ => anyhow::bail!("--tls-cert 与 --tls-key 必须同时给出"),
        }),
    };
    let scheme = if tls_material.is_some() {
        "https"
    } else {
        "http"
    };

    let state_key = *crypto::hkdf_derive(&k_manifest, b"web-state", "web-state");
    let app = Arc::new(App {
        payload: payload.to_vec(),
        k_manifest,
        state_key,
        answers_dir: cli.answers_dir.clone(),
        state_dir,
        forced_session: cli.session,
        clock: server_clock,
        time_mode: cli.time_check,
        login_rate_limit: cli.login_rate_limit,
        conns: std::sync::atomic::AtomicUsize::new(0),
        gate: Gate::new(4),
        event_log: Mutex::new(()),
        write_locks: Mutex::new(HashMap::new()),
        tmp_seq: std::sync::atomic::AtomicU64::new(0),
        state: Mutex::new(State::default()),
        windows,
        finalize_retry: Mutex::new(HashMap::new()),
        review: header.student_review,
        tls: tls_material.as_ref().map(|m| Arc::clone(&m.config)),
        secure_cookies: tls_material.is_some(),
    });

    let listener = bind_listener(&cli.bind, cli.port)?;

    // 明文跳转端口**先绑定、后打印**：绑定失败（8080 被占用）时，横幅里必须写明
    // 「旧书签不会自动跳」，否则老师以为旧书签还能用，学生打开就是报错页。
    let mut redirect_error: Option<String> = None;
    let redirect_listener = if tls_material.is_some() {
        match cli.http_redirect_port {
            0 => None,
            p if p == cli.port => {
                println!("⚠ --http-redirect-port 与 --port 相同（{p}）：已自动关闭明文跳转端口");
                None
            }
            p => match bind_listener(&cli.bind, p) {
                Ok(redirect) => Some(redirect),
                Err(e) => {
                    redirect_error = Some(format!("{e:#}"));
                    None
                }
            },
        }
    } else {
        if cli.http_redirect_port != 0 {
            println!("⚠ 当前是明文 HTTP（--tls off）：--http-redirect-port 将被忽略");
        }
        None
    };
    // 告诉学生该访问哪个地址：绑 0.0.0.0 / [::] 时用本机局域网 IP（教师机可能有多个），
    // 绑了具体地址（例如只绑 LAN、或只绑 127.0.0.1 调试）就直接用那个地址——
    // 否则绑了回环还会打印一个学生根本连不上的局域网 URL。
    println!(
        "考试服务已启动：{scheme}://{}:{}/",
        url_host(&bind_host(&cli.bind)),
        cli.port
    );
    if let Some(material) = &tls_material {
        if let Some(check) = &cert_check {
            if check.warnings.is_empty() {
                println!(
                    "外部证书自检通过（私钥匹配、EKU serverAuth、SAN 覆盖：{}）",
                    check.ok_hosts.join(", ")
                );
            } else {
                // 警告已经在上面逐条打过；这里补一句「对学生意味着什么」。
                println!("外部证书自检：{} 项警告（见上）", check.warnings.len());
                println!("⚠ 学生访问这些地址会看到更严重的证书警告（仍可点继续）");
            }
        }
        if material.generated {
            // 权限收紧可能失败（FAT32/网络盘），横幅不能无条件写「仅当前用户可读」。
            let key_hint = if material.key_hardened {
                "仅当前用户可读"
            } else {
                "⚠ 未能收紧权限（见上方告警）"
            };
            println!(
                "已自动生成自签证书（365 天）：{}（私钥 {}，{key_hint}）",
                material.cert_path.display(),
                material.key_path.display()
            );
        }
        println!("证书指纹（SHA-256）：{}", material.fingerprint);
        if !material.hosts.is_empty() {
            println!("SAN：{}", material.hosts.join(", "));
        }
        if material.not_after_unix > 0 {
            println!(
                "证书到期：{}（UTC；到期前 30 天重启会自动重签）",
                format_utc_datetime(material.not_after_unix)
            );
        }
        println!("学生首次打开会提示「不安全」→ 点「高级 → 继续前往」；指纹应与这里一致");
        if let Some(err) = &redirect_error {
            println!(
                "⚠ 旧书签自动跳转未启动（端口 {}）：{err}",
                cli.http_redirect_port
            );
        } else if redirect_listener.is_some() {
            println!(
                "旧书签 http://{}:{} 会自动跳到 HTTPS",
                url_host(&bind_host(&cli.bind)),
                cli.http_redirect_port
            );
        }
    } else {
        println!("⚠ 当前是明文 HTTP（--tls off）：同网段可嗅探，仅建议排障时使用");
    }
    println!(
        "场次 #{session_id}；答题卡输出到 {}",
        cli.answers_dir.display()
    );
    println!("（阶段 1：作答实时落盘、崩溃后重登可恢复；Ctrl+C 结束）");

    {
        let sweep = Arc::clone(&app);
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(15));
                // 单轮 panic 不能带走整个后台线程：否则到点再也不收卷，直到有人重启服务。
                let tick = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    sweep_tick(&sweep);
                }));
                if tick.is_err() {
                    eprintln!("⚠ 自动收卷本轮 panic（已捕获，下一轮继续）");
                }
            }
        });
    }

    if let Some(redirect) = redirect_listener {
        // 只做跳转的明文端口：任何请求都回 301 到 HTTPS。它不返回页面、不碰状态，
        // 所以「明文端口泄漏答案」的问题不存在——泄漏只发生在真正的数据请求上。
        let host = bind_host(&cli.bind);
        let https_port = cli.port;
        std::thread::spawn(move || redirect_loop(redirect, host, https_port));
    }

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let app = Arc::clone(&app);
                let tls_mode = app.tls.is_some();
                std::thread::spawn(move || {
                    if let Err(e) = handle(app, stream) {
                        if tls_mode {
                            log_tls_error(&e);
                        } else {
                            eprintln!("连接处理失败：{e:#}");
                        }
                    }
                });
            }
            Err(e) => eprintln!("accept 失败：{e}"),
        }
    }
    Ok(())
}

/// 证书 SAN 里的地址：`--tls-host` 优先；默认本机局域网 IP + 回环 + localhost。
///
/// 少一个就会让浏览器报「名称不匹配」——比自签本身更吓人，所以宁可多列几个。
fn tls_hosts(cli: &Cli) -> Vec<String> {
    let mut hosts = if cli.tls_hosts.is_empty() {
        vec![local_ip(), "127.0.0.1".to_string(), "localhost".to_string()]
    } else {
        cli.tls_hosts.clone()
    };
    // 只绑具体地址（多网卡教师机只绑机房侧网卡，v1.4.7 起的推荐用法）时，
    // 该地址必须进 SAN —— 否则横幅打印 `https://<那个地址>:8443/`，
    // 学生看到的却是「证书名称不匹配」，比自签本身更吓人。
    if !matches!(cli.bind.as_str(), "0.0.0.0" | "::" | "[::]") {
        let bound = bind_host(&cli.bind);
        if !hosts.iter().any(|h| h == &bound) {
            hosts.push(bound);
        }
    }
    // 重复的 `--tls-host` 会产生重复告警（同一个地址报两次），按规范化形式去重：
    // `LOCALHOST` 与 `localhost`、`0:0:0:0:0:0:0:1` 与 `::1` 视为同一个地址。
    let mut seen = std::collections::HashSet::new();
    hosts.retain(|h| seen.insert(tls::normalize_host(h)));
    hosts
}

/// URL 里的主机部分：IPv6 字面量必须加方括号（`https://[::1]:8443/`）。
///
/// 裸写 `https://::1:8443/` 不是合法 URL（浏览器会解析失败），而 `--bind ::1`
/// 是这个程序**允许**的用法（教师本机调试/纯 IPv6 局域网），所以必须统一处理。
fn url_host(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

/// 明文跳转端口最多同时处理多少连接。
///
/// 这个端口**不加密、不认证**，任何能连上的人都能开连接；主监听器有 256 上限，
/// 这里也要有——否则 8080 就成了「一个不停 spawn 线程」的后门，会和主服务抢 CPU。
const MAX_REDIRECT_CONNS: usize = 64;

/// 只做跳转的明文端口：每个连接读一行请求行、回 301 到 HTTPS，然后关闭。
///
/// 它**不返回页面、不碰状态**，所以「明文端口泄漏答案」的问题不存在——
/// 泄漏只会发生在真正的数据请求上，而那些请求只走 TLS 端口。
fn redirect_loop(listener: TcpListener, host: String, https_port: u16) {
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(e) => {
                eprintln!("明文跳转端口 accept 失败：{e}");
                continue;
            }
        };
        if active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= MAX_REDIRECT_CONNS {
            active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            // 超限直接丢连接：跳转不是必需功能，别让它拖垮主服务。
            continue;
        }
        struct Slot(Arc<std::sync::atomic::AtomicUsize>);
        impl Drop for Slot {
            fn drop(&mut self) {
                self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let _slot = Slot(Arc::clone(&active));
        let host = host.clone();
        std::thread::spawn(move || {
            let _slot = _slot;
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            handle_redirect_stream(&mut stream, &host, https_port);
        });
    }
}

/// 单个明文连接 → 301。抽成函数是为了能直接测（复制一份逻辑的用例毫无判别力）。
fn handle_redirect_stream(stream: &mut TcpStream, host: &str, https_port: u16) {
    let mut buf = [0u8; 2048];
    let n = stream.read(&mut buf).unwrap_or(0);
    let path = std::str::from_utf8(&buf[..n])
        .ok()
        .and_then(|text| text.lines().next().map(str::to_string))
        .and_then(|line| line.split_whitespace().nth(1).map(str::to_string))
        .unwrap_or_else(|| "/".to_string());
    let location = format!("https://{}:{https_port}{path}", url_host(host));
    // 只加 nosniff：301 没有 body，CSP/frame-ancestors 对它没有意义
    // （这里也没走 `write_response`，因为那是给「有内容」的响应准备的）。
    let resp = format!(
        "HTTP/1.1 301 Moved Permanently\r\nLocation: {location}\r\nX-Content-Type-Options: nosniff\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

/// 把 Unix 秒格式化成 `YYYY-MM-DD HH:MM`（UTC）。
///
/// 证书的 `notAfter` 本来就是 UTC 语义，这里也标 UTC，避免老师按本地时区误读。
fn format_utc_datetime(unix: u64) -> String {
    let Ok(dt) = time::OffsetDateTime::from_unix_timestamp(unix as i64) else {
        return "未知".to_string();
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        dt.year(),
        u8::from(dt.month()),
        dt.day(),
        dt.hour(),
        dt.minute()
    )
}

/// TLS 端口上的连接级错误计数（用于日志采样）。
static CONN_ERRORS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// TLS 端口上的连接错误：前 5 次完整打印，之后每 100 次抽样一行。
///
/// 为什么采样：`https://` 端口被 `http://` 访问、或端口扫描，都会稳定产生一条错误；
/// 全班几十台设备 + 扫描器就能把日志刷满、并把真正的故障淹掉。
fn log_tls_error(e: &anyhow::Error) {
    use std::sync::atomic::Ordering;
    let n = CONN_ERRORS.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= 5 || n.is_multiple_of(100) {
        eprintln!(
            "连接处理失败（第 {n} 次；可能是用 http:// 访问了 HTTPS 端口，或端口扫描）：{e:#}"
        );
    }
}

fn local_ip() -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".into())
}

/// 启动横幅里用的「学生应访问的主机名」。
///
/// 通配地址（`0.0.0.0` / `[::]`）本身不可访问，要换成教师机的局域网地址；
/// 绑定了具体地址时就用它——绑定回环却打印局域网 IP，会让学生照着连不上。
fn bind_host(bind: &str) -> String {
    match bind {
        "0.0.0.0" | "::" | "[::]" => local_ip(),
        other => other.to_string(),
    }
}

/// 绑定监听端口。
///
/// 抽成函数是为了让「端口被占用」这条失败分支能被测试直接覆盖——`main` 里没法在
/// 同一进程内先占住端口再调一次 `bind`。错误信息要说清「怎么办」：占用是现场最常见的
/// 启动失败原因，只抛 `Address already in use` 老师看不懂。
fn bind_listener(bind: &str, port: u16) -> Result<TcpListener> {
    let addr = format!("{bind}:{port}");
    TcpListener::bind(&addr).with_context(|| {
        format!("监听 {addr} 失败：端口可能已被其他程序占用（换端口或先关掉占用它的程序）")
    })
}

/// 启动时收紧状态目录，并给升级前已经落盘的旧状态文件补一次权限。
///
/// 目录上的继承位决定「之后新建的文件自动受限」，但**之前**写下的文件仍带着旧
/// ACL（例如 Everyone 可读），只收紧目录不会回收它们。这里逐个补一遍。
///
/// 失败只告警：状态内容本身是加密的，权限是第二道防线；因为权限问题拒绝开考，
/// 造成的损失比风险本身更大。
fn harden_state_dir(dir: &std::path::Path) {
    if let Err(e) = secure_fs::harden_dir(dir) {
        eprintln!("⚠ 状态目录权限收紧失败 {}：{e}", dir.display());
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        // 写盘失败可能留下 `*.json.<seq>.tmp`：它永远不是权威副本，启动时直接清掉。
        if name.ends_with(".tmp") {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Err(e) = secure_fs::harden_file(&path) {
            eprintln!("⚠ 旧状态文件权限收紧失败 {}：{e}", path.display());
        }
    }
}

fn handle(app: Arc<App>, mut stream: TcpStream) -> Result<()> {
    use std::sync::atomic::Ordering;
    if app.conns.fetch_add(1, Ordering::SeqCst) >= 256 {
        app.conns.fetch_sub(1, Ordering::SeqCst);
        return Ok(());
    }
    struct Guard<'a>(&'a std::sync::atomic::AtomicUsize);
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let _guard = Guard(&app.conns);
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    // 写也要有超时：对端不读、发送缓冲又满时，`close_notify` 的 flush 会无限阻塞，
    // 线程就一直挂在这个连接上（读超时管不到写）。
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    // 超时要设在**裸 TCP** 上：TLS 握手与后续读取都受它约束（StreamOwned 不暴露 set_read_timeout）。
    //
    // 再克隆一个句柄留给 `drain_briefly` 临时把超时降到 200 ms：克隆的是同一个 socket，
    // 改超时对 TLS 会话同样生效。否则「声明超大 Content-Length 却不发数据」能把线程占满 10 秒。
    let ctl = stream.try_clone().ok();
    let peer = peer_ip(&stream);
    match app.tls.clone() {
        Some(config) => {
            let conn = rustls::ServerConnection::new(config)
                .context("创建 TLS 会话失败（证书配置有问题？）")?;
            let mut tls = rustls::StreamOwned::new(conn, stream);
            let result = serve(&app, &mut tls, peer, ctl.as_ref());
            // 显式发 close_notify：只写 FIN 的话，部分客户端会把 EOF 当「响应被截断」。
            tls.conn.send_close_notify();
            let _ = tls.flush();
            result
        }
        None => serve(&app, &mut stream, peer, ctl.as_ref()),
    }
}

/// 处理一条已建立（可能已加密）的连接的请求。
///
/// 泛化成 `Read + Write` 是为了同一套代码同时跑明文与 TLS；`peer` 由调用方从裸 TCP 取。
fn serve(
    app: &App,
    stream: &mut (impl Read + Write),
    peer: IpAddr,
    ctl: Option<&TcpStream>,
) -> Result<()> {
    match read_request(stream, peer)? {
        ReadOutcome::Closed => Ok(()),
        ReadOutcome::Oversized(limit) => {
            // 先回状态码，再把没读的 body 短暂读掉：直接 close 会发 RST，
            // 客户端可能只看到「连接被重置」而不是 413。
            let resp = Response::error(413, format!("请求体超过 {limit} 字节"));
            let _ = write_response(stream, resp);
            // 拿不到控制句柄（try_clone 失败）就干脆不 drain：宁可让客户端看到 RST，
            // 也不要把线程押在 10 秒的读超时上（那正是 drain 想避免的占用）。
            if ctl.is_some() {
                with_short_read_timeout(ctl, || drain_briefly(stream));
            }
            Ok(())
        }
        ReadOutcome::Chunked => {
            let resp = Response::error(411, "不支持分块传输（请带上 Content-Length）");
            let _ = write_response(stream, resp);
            if ctl.is_some() {
                with_short_read_timeout(ctl, || drain_briefly(stream));
            }
            Ok(())
        }
        ReadOutcome::Request(req) => write_response(stream, route(app, &req)),
    }
}

/// 把读超时临时降到 200 ms 执行 `f`，结束后恢复 10 s。
///
/// 为什么需要：`drain_briefly` 是「尽力把没读的 body 读掉」，绝不能为了它把线程占住
/// 10 秒——恶意客户端发一个「声明 100 MiB、实际不发」的请求就能刷满 256 个连接。
fn with_short_read_timeout(ctl: Option<&TcpStream>, f: impl FnOnce()) {
    if let Some(ctl) = ctl {
        let _ = ctl.set_read_timeout(Some(Duration::from_millis(200)));
        f();
        let _ = ctl.set_read_timeout(Some(Duration::from_secs(10)));
    } else {
        f();
    }
}

fn route(app: &App, req: &Request) -> Response {
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => Response::html_owned(index_html(app)),
        ("GET", "/assets/app.css") => {
            Response::asset("text/css; charset=utf-8", include_str!("../assets/app.css"))
        }
        // 行内脚本/样式拆到这里，是为了能上 CSP（`script-src 'self'`）——
        // 行内 `<script>` 只能靠 `'unsafe-inline'` 放行，等于没加 CSP。
        ("GET", "/assets/app.js") => Response::asset(
            "text/javascript; charset=utf-8",
            include_str!("../assets/app.js"),
        ),
        ("GET", "/assets/codemirror.css") => Response::asset(
            "text/css; charset=utf-8",
            include_str!("../assets/codemirror.min.css"),
        ),
        ("GET", "/assets/codemirror.js") => Response::asset(
            "text/javascript; charset=utf-8",
            include_str!("../assets/codemirror.min.js"),
        ),
        ("GET", "/assets/clike.js") => Response::asset(
            "text/javascript; charset=utf-8",
            include_str!("../assets/clike.min.js"),
        ),
        ("GET", "/assets/python.js") => Response::asset(
            "text/javascript; charset=utf-8",
            include_str!("../assets/python.min.js"),
        ),
        ("POST", "/api/login") => login(app, req),
        ("GET", "/api/paper") => paper(app, req),
        ("PUT", "/api/answer") => save_answer(app, req),
        ("POST", "/api/submit") => submit(app, req),
        ("POST", "/api/logout") => logout(app, req),
        ("GET", "/api/time") => time_info(app, req),
        _ => Response::error(404, "not found"),
    }
}

fn login(app: &App, req: &Request) -> Response {
    // 登录也要求 CSRF 头：否则任何网页都能用「别人的学号」发一个跨站 POST，
    // 把受害者的浏览器登进攻击者的答题会话（登录 CSRF）。前端所有请求都带这个头。
    if !csrf_ok(req) {
        return Response::error(403, "缺少 CSRF 头");
    }
    // 只信 TCP 对端地址：x-forwarded-for 可伪造，用来绕过 IP 限速。
    let ip = req.peer;

    let body: Value = match serde_json::from_slice(&req.body) {
        Ok(v) => v,
        Err(_) => return Response::error(400, "请求不是合法 JSON"),
    };
    let Some(student_id) = body.get("student_id").and_then(Value::as_u64) else {
        return Response::error(400, "缺少 student_id");
    };
    let pin = body.get("pin").and_then(Value::as_str).map(str::to_string);

    // 按 IP 的计数放在「请求体解析成功、学号已拿到」之后。
    //
    // 为什么：同一 NAT 后面全班共用一个 IP 桶。旧客户端、浏览器插件、扫描器发几个
    // 坏请求就把 60 个名额占满，真实学生会被 429 挡在门外一分钟（现场表现：
    // 「全班突然都登不上，过一会儿又好了」）。挡 PIN 爆破靠的是下面的「每学号 10 次」。
    {
        let mut state = app.lock_state();
        let now = app.now();
        let entry = state.attempts.entry(ip).or_insert((0, now));
        if now.saturating_sub(entry.1) > 60 {
            *entry = (0, now);
        }
        if app.login_rate_limit != 0 && entry.0 >= app.login_rate_limit {
            return Response::error(429, "登录尝试过多，请一分钟后再试");
        }
        entry.0 += 1;
    }

    {
        let mut state = app.lock_state();
        let now = app.now();
        let entry = state.attempts_student.entry(student_id).or_insert((0, now));
        if now.saturating_sub(entry.1) > 60 {
            *entry = (0, now);
        }
        if entry.0 >= 10 {
            return Response::error(429, "该学号登录尝试过多，请一分钟后再试");
        }
        entry.0 += 1;
    }

    let unlocked = {
        // 槽位守卫在作用域结束时归还（panic 也不会泄漏，见 GateSlot）。
        let _slot = app.gate.acquire();
        app.unlock(student_id, pin.as_deref())
    };

    match unlocked {
        Ok((token, mut student)) => {
            // 断线重连：从磁盘恢复已保存答案与交卷状态。
            if let Some((answers, submitted, exported)) =
                app.load_state(student_id, student.session_id)
            {
                student.answers = answers;
                student.submitted = submitted;
                student.exported = exported;
            }
            let mut state = app.lock_state();
            if let Some(old) = state.tokens_by_student.insert(student_id, token.clone()) {
                state.students.remove(&old);
            }
            state.students.insert(token.clone(), student);
            Response::json(200, json!({"ok": true})).with_cookie(session_cookie(app, &token))
        }
        Err(msg) => Response::error(401, msg),
    }
}

/// 主动退出：清 cookie，并把 token 从内存状态里删掉（旧 cookie 立刻 401）。
///
/// 为什么服务端也要删：只清 cookie 的话，拷走 cookie 的人还能继续用这个会话。
/// 答案早已在磁盘上，删掉内存里的 Student 不影响重登恢复。
fn logout(app: &App, req: &Request) -> Response {
    if !csrf_ok(req) {
        return Response::error(403, "缺少 CSRF 头");
    }
    if let Some(token) = cookie_token(req) {
        let mut state = app.lock_state();
        if let Some(student) = state.students.remove(&token) {
            state.tokens_by_student.remove(&student.student_id);
        }
    }
    Response::json(200, json!({"ok": true})).with_cookie(expired_session_cookie(app))
}

fn csrf_ok(req: &Request) -> bool {
    req.headers
        .get("x-ees-csrf")
        .map(|v| v == "1")
        .unwrap_or(false)
}

impl App {
    /// 判定用的「现在」：本机时间 + 启动时算出的网络校准偏移。
    ///
    /// 所有场次窗口、限速窗口、最后活跃时间都走这里，不直接读系统时钟——
    /// 否则 `--time-check` 校准出来的偏移只在挑场次时生效一次，等于没校准。
    fn now(&self) -> u64 {
        self.clock.now()
    }

    /// 取状态锁；**中毒也继续**。
    ///
    /// 为什么容忍中毒：生产代码有 10 处取锁，任何一次持锁 panic 都会把锁永久毒化，
    /// 之后每个请求线程 panic、sweep 线程也死——进程还在监听但全站不可用。
    /// 宁可带着可能不一致的状态继续，也不要让整场考试直接死掉。
    fn lock_state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn unlock(&self, student_id: u64, pin: Option<&str>) -> Result<(String, Student), String> {
        let (header, blob) = container::parse_header(&self.payload, &self.k_manifest)
            .map_err(|e| format!("包解析失败：{e}"))?;
        let now = self.now();
        let ids: Vec<u32> = match self.forced_session {
            Some(id) => vec![id],
            None => header.sessions.iter().map(|s| s.session_id).collect(),
        };
        let salt = hex::decode(&header.salt).map_err(|e| format!("salt 解码失败：{e}"))?;
        for sid in ids {
            let Some(session) = header.sessions.iter().find(|s| s.session_id == sid) else {
                continue;
            };
            // 严格解析：包内时间必须带时区偏移（与启动时的 `session_times` 同口径）。
            let Ok((start, end)) =
                container::parse_session_window(session.session_id, &session.start, &session.end)
            else {
                continue;
            };
            // 与客户端 `decide_session` 统一为 [start, end)：同一秒 now == end 时
            // 两端都算「已结束」，否则网页端能比离线端多答一秒。
            if !window_open(now, start, end) {
                continue;
            }
            let Ok(Some(unlocked)) = container::try_unlock_session_with_pin(
                session, &header, blob, student_id, &salt, pin,
            ) else {
                continue;
            };
            let data = container::decrypt_data(&unlocked.session_secret, &salt, &unlocked.data_ct)
                .map_err(|e| format!("解密题库失败：{e}"))?;
            let paper = if unlocked.sections.is_empty() {
                picker::generate_paper_ranged(
                    student_id,
                    unlocked.session_id,
                    &salt,
                    &data.banks,
                    |exam_id, fallback| (data.pick_for(exam_id, fallback), data.range_for(exam_id)),
                )
            } else if unlocked.sections.iter().any(|s| !s.parts.is_empty()) {
                picker::generate_paper_parts(
                    student_id,
                    unlocked.session_id,
                    &salt,
                    &data.banks,
                    &unlocked.sections,
                )
            } else {
                picker::generate_paper_sections(
                    student_id,
                    unlocked.session_id,
                    &salt,
                    &data.banks,
                    &unlocked.sections,
                )
            }
            .map_err(|e| format!("抽题失败：{e}"))?;
            let mut token_bytes = [0u8; 32];
            rand::rng().fill_bytes(&mut token_bytes);
            let token = hex::encode(token_bytes);
            let student = Student {
                student_id,
                session_id: unlocked.session_id,
                paper,
                answers: BTreeMap::new(),
                submitted: false,
                exported: None,
                last_seen: now,
            };
            return Ok((token, student));
        }
        Err("学号或 PIN 不正确，或不在考试时间窗内".into())
    }
}

fn cookie_token(req: &Request) -> Option<String> {
    req.headers.get("cookie")?.split(';').find_map(|part| {
        let part = part.trim();
        part.strip_prefix("ees_token=").map(str::to_string)
    })
}

/// 首页 HTML。TLS 模式下插入一行提示：学生看到「不安全」时该怎么点。
///
/// 用占位替换而不是两套模板：页面只有一份，改动不会漂。
fn index_html(app: &App) -> String {
    // TLS 与明文各一条：明文模式下如果什么都不显示，页面看起来和 HTTPS 一模一样，
    // 学生/监考都无从判断「现在到底加没加密」。
    let hint = if app.secure_cookies {
        "<div class=\"note\">本页使用考场内网自签证书：若浏览器提示「不安全」，请点「高级 → 继续前往」；证书指纹应与教师屏幕一致。iPad/iPhone 请用 Safari（Chrome 无法绕过证书警告）。</div>"
    } else {
        "<div class=\"note\">⚠ 当前为明文 HTTP（老师用 --tls off 启动）：仅限排障；请让学生先和监考确认后再作答。</div>"
    };
    INDEX_HTML.replace("<!--TLS_HINT-->", hint)
}

fn paper(app: &App, req: &Request) -> Response {
    let Some(token) = cookie_token(req) else {
        return Response::error(401, "未登录");
    };
    let mut state = app.lock_state();
    let Some(st) = state.students.get_mut(&token) else {
        return Response::error(401, "登录已失效");
    };
    let now = app.now();
    st.last_seen = now;
    let (start, end) = session_window(app, st.session_id).unwrap_or((0, 0));
    let expired = end != 0 && !window_open(now, start, end);
    let allow_review = app.review.allows_review(st.submitted, expired);
    let mut base = json!({
        "student_id": st.student_id,
        "session_id": st.session_id,
        "server_time": now,
        "start_unix": start,
        "end_unix": end,
        "submitted": st.submitted,
        "review": review_name(app.review),
    });
    // 已交卷且策略不允许回看：**不下发卷面**。
    //
    // 离线端一直是这么做的（`review_available`）；网页端以前完全忽略这个策略——
    // 老师按默认打包（「仅教师可查看原卷」）时，网页端交卷后照样能看到全卷。
    if st.submitted && !allow_review {
        if let Some(map) = base.as_object_mut() {
            map.insert("paper".into(), Value::Null);
            map.insert("answers".into(), Value::Null);
            map.insert("answer_text".into(), Value::Null);
        }
        return Response::json(200, base).with_cookie(session_cookie(app, &token));
    }
    let answer_text: HashMap<&str, String> = st
        .paper
        .sections
        .iter()
        .flat_map(|section| &section.items)
        .filter_map(|item| {
            let raw = st.answers.get(&item.id)?;
            Some((item.id.as_str(), answer_text(item, raw)))
        })
        .collect();
    if let Some(map) = base.as_object_mut() {
        map.insert("paper".into(), json!(st.paper));
        map.insert("answers".into(), json!(st.answers));
        map.insert("answer_text".into(), json!(answer_text));
    }
    // 每次取卷都续一次期：学生只要还在答题，会话就不会因为「60 分钟」被掐断。
    Response::json(200, base).with_cookie(session_cookie(app, &token))
}

/// 回看策略的线上表示。
fn review_name(policy: common::model::StudentReviewPolicy) -> &'static str {
    match policy {
        common::model::StudentReviewPolicy::None => "none",
        common::model::StudentReviewPolicy::Free => "free",
    }
}

/// 把题库里存的原始答案还原成「网页输入框里该显示的文字」。
///
/// 填空/改错在存储层是 JSON（离线端的编码），直接塞进 textarea 会显示成
/// `["a","b"]`——学生一改就会被二次编码，那题必错。
fn answer_text(item: &picker::PaperItem, raw: &str) -> String {
    match item.qtype.as_str() {
        "fill" => answer_sheet::decode_parts(raw).join("\n"),
        "fix" => answer_sheet::decode_fixes(raw)
            .iter()
            .map(|(line, answer)| format!("{line} {answer}").trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => raw.to_string(),
    }
}

/// 填空/改错在离线端是 JSON 编码；网页端按「每行一个」转换。
///
/// 填空按 `blank_count` 补齐空行：学生只填前两个空时长度会短于期望值，
/// 补齐后判分口径与离线端一致（缺的算错）。
fn normalize_answer(item: &picker::PaperItem, value: &str) -> String {
    match item.qtype.as_str() {
        "fill" => {
            let mut parts: Vec<String> = value
                .split('\n')
                .map(|line| line.trim_end_matches('\r').to_string())
                .collect();
            if let common::model::QuestionBody::Template { blank_count, .. } = item.body
                && blank_count > 0
                && parts.len() < blank_count as usize
            {
                parts.resize(blank_count as usize, String::new());
            }
            answer_sheet::encode_parts(&parts)
        }
        "fix" => {
            let mut fixes: Vec<(String, String)> = Vec::new();
            for line in value.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let (no, ans) = line.split_once([' ', '\t', ':']).unwrap_or((line, ""));
                fixes.push((no.trim().to_string(), ans.trim().to_string()));
            }
            answer_sheet::encode_fixes(&fixes)
        }
        _ => value.to_string(),
    }
}

/// 某场次的时间窗；查启动时缓存的表。
///
/// 返回 `Option` 而不是 `(0, 0)`：缺失是「缓存与 payload 不同源」的实现错误，
/// 用 0 隐式表达会让三个调用方各自猜（有人放行、有人从不收卷）。
fn session_window(app: &App, session_id: u32) -> Option<(u64, u64)> {
    app.windows.get(&session_id).copied()
}

/// 从 header 算出所有场次的时间窗（启动时调用一次）。
///
/// 用 `container::parse_session_window`：严格解析（必须显式时区）+ 结束晚于开始。
/// 构建期与运行期共用同一个入口，避免「构建期严、运行期松」。
fn session_windows_from(header: &container::Header) -> Result<HashMap<u32, (u64, u64)>, String> {
    windows_from_sessions(&header.sessions)
}

/// `session_windows_from` 的纯函数内核（单独拆出来是为了能直接构造 header 断言错误路径）。
fn windows_from_sessions(
    sessions: &[container::SessionHeader],
) -> Result<HashMap<u32, (u64, u64)>, String> {
    let mut windows = HashMap::new();
    for session in sessions {
        windows.insert(
            session.session_id,
            container::parse_session_window(session.session_id, &session.start, &session.end)?,
        );
    }
    Ok(windows)
}

fn save_answer(app: &App, req: &Request) -> Response {
    if !csrf_ok(req) {
        return Response::error(403, "缺少 CSRF 头");
    }
    let Some(token) = cookie_token(req) else {
        return Response::error(401, "未登录");
    };
    let body: Value = match serde_json::from_slice(&req.body) {
        Ok(v) => v,
        Err(_) => return Response::error(400, "请求不是合法 JSON"),
    };
    let Some(qid) = body.get("qid").and_then(Value::as_str) else {
        return Response::error(400, "缺少 qid");
    };
    let Some(value) = body.get("value").and_then(Value::as_str) else {
        return Response::error(400, "缺少 value");
    };
    if value.len() > 1024 * 1024 {
        return Response::error(413, "单题答案超过 1 MiB");
    }

    // 校验与写入内存都在锁内完成；**克隆答案**也只是内存操作，真正的写盘放到锁外。
    // 交卷路径在 v1.4.4 已经把写盘挪出锁，保存路径同理（它是全场频率最高的写操作）。
    let (student_id, session_id, answers) = {
        let mut state = app.lock_state();
        let Some(st) = state.students.get_mut(&token) else {
            return Response::error(401, "登录已失效");
        };
        if st.submitted {
            return Response::error(409, "已交卷，不能再修改");
        }
        let Some((start, end)) = session_window(app, st.session_id) else {
            // 缓存与包不同源属实现错误：宁可拒绝写，也不要放行「没有窗口」的答案。
            return Response::error(500, "场次信息缺失（服务端内部错误，请联系监考）");
        };
        if !window_open(app.now(), start, end) {
            return Response::error(409, "考试已结束");
        }
        let Some(item) = st
            .paper
            .sections
            .iter()
            .flat_map(|s| &s.items)
            .find(|i| i.id == qid)
        else {
            return Response::error(400, "题目不在本卷中");
        };
        // 填空/改错的编码转换（与 paper 下发的 `answer_text` 互逆）。
        let normalized = normalize_answer(item, value);
        let value = normalized.as_str();
        // 先算「换成这一题之后」的总量，再决定写不写：v1.4.3 及以前是先 insert 再算总量，
        // 返回 413 时那份超限答案已经留在内存里了——下次交卷会被写进答题卡与状态文件、
        // 重启后还会被恢复，「拒绝」等于没拒绝。
        let previous = st.answers.get(qid).map_or(0, String::len);
        let total: usize = st
            .answers
            .values()
            .map(String::len)
            .sum::<usize>()
            .saturating_sub(previous)
            .saturating_add(value.len());
        if total > 16 * 1024 * 1024 {
            return Response::error(413, "整卡答案超过 16 MiB");
        }
        st.answers.insert(qid.to_string(), value.to_string());
        st.last_seen = app.now();
        (st.student_id, st.session_id, st.answers.clone())
    };

    // 每次保存都落盘（v1.4.5）。
    //
    // 为什么：交卷前答案只在内存里时，教师机崩溃、断电、误杀进程都会让**整场作答全部丢失**，
    // 而文档一直写着「答案实时落盘」。现在每次保存写一份加密状态（原子替换），重登即可恢复。
    //
    // 这里用 durable=false：保存是前端 600ms 防抖一次的高频操作，每次都 fsync 会把磁盘打满；
    // 进程崩溃不丢（页缓存还在），只有断电可能丢最近几秒——仍远好于「只在交卷时落盘」。
    match app.persist_state(
        Some(&token),
        student_id,
        session_id,
        &answers,
        false,
        None,
        false,
    ) {
        Ok(PersistOutcome::Written) => Response::json(200, json!({"ok": true})),
        // 写盘期间被交卷抢占了：以内存为准，这次保存不算数（客户端该停手了）。
        Ok(PersistOutcome::SkippedSubmitted) => Response::error(409, "已交卷，不能再修改"),
        // 内存里已经有了，但磁盘上没有：必须让学生知道，让他重试这一题。
        Err(e) => Response::error(
            500,
            format!("答案已收下但状态写盘失败：{e}（请重试这一题）"),
        ),
    }
}

fn submit(app: &App, req: &Request) -> Response {
    if !csrf_ok(req) {
        return Response::error(403, "缺少 CSRF 头");
    }
    let Some(token) = cookie_token(req) else {
        return Response::error(401, "未登录");
    };

    // 第一段（持锁）：判重 + 抢占 + 克隆快照。
    //
    // 交卷是全场最容易撞车的一刻（下课铃一响所有人一起点），所以这里和自动收卷一样
    // 把「加密 + 写盘」放到锁外：否则 50 个人同时交卷时，第一个人加密答题卡的几十毫秒里
    // 其余 49 个请求全堵在锁上，页面表现为「点了没反应」。
    let snapshot = {
        let mut state = app.lock_state();
        let Some(st) = state.students.get_mut(&token) else {
            return Response::error(401, "登录已失效");
        };
        if st.submitted {
            if let Some(name) = st.exported.clone() {
                // 不能只看「文件存在」：断电可能留下半截卡，那样学生会永远交不了卷。
                if is_valid_sheet(app, &app.answers_dir.join(&name)) {
                    return Response::json(200, json!({"ok": true, "already": true, "file": name}))
                        .with_cookie(post_submit_cookie(app, &token));
                }
                // 状态说已交卷，但答题卡文件不见了（被手工删掉、盘符换了、目录被清）。
                // 退回去重写一份：学生没有卡比重复写一份严重得多。
                st.exported = None;
                st.submitted = false;
            } else {
                // `submitted=true` 但还没有 `exported`：后台 sweep 正在封存（它先置位、
                // 再写盘）。这时返回 200 会让前端显示「已交卷」，而那一刻卡还没落盘；
                // 若 sweep 随后失败还会回滚状态。返回 503 让前端提示「正在封存，请稍候」并重试。
                return Response::error(503, "正在封存答题卡，请稍候");
            }
        }
        // 抢占：先把 submitted 置位，避免与自动收卷同时冻结（会写出两份同名校验一致的
        // .eea，事件日志也会重复）。失败时回滚，见第三段。
        st.submitted = true;
        FreezeSnapshot {
            token: token.clone(),
            student_id: st.student_id,
            session_id: st.session_id,
            paper: st.paper.clone(),
            answers: st.answers.clone(),
        }
    };

    // 第二段（不持锁）：密封 + 写盘 + 写状态 + 记事件。
    let now = app.now();
    let result = seal_and_export(
        app,
        snapshot.student_id,
        snapshot.session_id,
        &snapshot.paper,
        &snapshot.answers,
        now,
    );

    // 第三段（持锁）：回填结果。
    let mut state = app.lock_state();
    let Some(st) = state.students.get_mut(&snapshot.token) else {
        // 锁外被重新登录顶掉：卡已经写出、事件也记了，无需回填。
        return match result {
            Ok(name) => Response::json(200, json!({"ok": true, "file": name})),
            Err(e) => Response::error(500, e),
        };
    };
    match result {
        Ok(name) => {
            st.exported = Some(name.clone());
            st.last_seen = now;
            Response::json(200, json!({"ok": true, "file": name}))
                .with_cookie(post_submit_cookie(app, &token))
        }
        Err(e) => {
            st.submitted = false; // 撤回抢占：让学生能再试一次
            // 交卷失败、学生还要继续答：会话必须留着。
            Response::error(500, e).with_cookie(session_cookie(app, &token))
        }
    }
}

/// 写文件并 `fsync`（交卷用）。
///
/// 必须用写句柄：只读句柄的 `sync_all()` 在 Windows 上返回 `ERROR_ACCESS_DENIED`，
/// 等于根本没有刷盘——v1.4.5 的「durable fsync」就是这么空转的。
fn write_file_durable(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut file = std::fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// 已有答题卡是否**真的可读**：用 `answer_sheet::open` 完整校验（头/版本/指纹/AAD/答案长度）。
///
/// 只比 8 字节魔数不够：断电可能留下「魔数对、内容截断」的文件，那种卡学生会永远交不了卷，
/// 而 `submit` 的幂等分支会一直返回「已交卷」。只有重复交卷时才会走到这里，
/// 一次解密（最坏 16 MiB，约 10 ms）可以接受。
fn is_valid_sheet(app: &App, path: &std::path::Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    common::answer_sheet::open(&app.k_manifest, &bytes).is_ok()
}

/// 目录 fsync：rename 的持久化需要它（POSIX）。Windows 没有对应语义，忽略错误。
fn fsync_dir(dir: Option<&std::path::Path>) {
    #[cfg(unix)]
    if let Some(dir) = dir {
        let _ = std::fs::File::open(dir).and_then(|f| f.sync_all());
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// 自动收卷连续失败后的重试间隔（秒）：15 s → 30 → 60 → 120 → 300 封顶。
///
/// 为什么要退避：失败通常是持续状态（磁盘满、目录只读）。不退避就是每 15 秒做一次
/// 「序列化 + AES-GCM + 写盘」，50 人规模下会持续加压并把日志刷满。
fn backoff_secs(failures: u32) -> u64 {
    const BASE: u64 = 15;
    const CAP: u64 = 300;
    BASE.saturating_mul(1u64 << failures.min(5)).min(CAP)
}

/// 生成答题卡 → 落盘 → 写状态 → 记事件，**全程不接触 `App::state` 锁**。
///
/// 交卷接口与自动收卷都走这里：加密答题卡（AES-GCM）、写 `.eea`、加密写状态文件、
/// 收紧文件 ACL，几十毫秒量级。攥着全局状态锁做的话（v1.4.3 及以前），这期间**所有
/// 学生**的每次请求都堵在锁上，表现为「页面点了没反应」且日志里什么都没有。
fn seal_and_export(
    app: &App,
    student_id: u64,
    session_id: u32,
    paper: &picker::Paper,
    answers: &BTreeMap<String, String>,
    now: u64,
) -> Result<String, String> {
    #[cfg(test)]
    seal_hook::notify(true);
    let mut sheet = answer_sheet::AnswerSheet::from_paper(paper, student_id, session_id, now);
    let qids: Vec<String> = paper
        .sections
        .iter()
        .flat_map(|s| s.items.iter().map(|i| i.id.clone()))
        .collect();
    for (idx, qid) in qids.iter().enumerate() {
        if let Some(v) = answers.get(qid) {
            sheet.set_answer(idx, Some(v.clone()), now);
        }
    }
    sheet.freeze(now);
    let bytes =
        answer_sheet::seal(&app.k_manifest, &sheet).map_err(|e| format!("生成答题卡失败：{e}"))?;
    let name = format!(
        "answers-{}-{}-{}.eea",
        student_id,
        session_id,
        &sheet.paper_fingerprint[..8]
    );
    // 先落卡、再写状态：状态里的「已交卷」只有在卡确实写成功之后才成立；
    // 而且必须 fsync——交卷瞬间断电若留下半截卡，而状态记着已交卷，学生就再也交不了。
    write_file_durable(&app.answers_dir.join(&name), &bytes)
        .map_err(|e| format!("写入答题卡失败：{e}"))?;
    app.persist_state(
        None,
        student_id,
        session_id,
        answers,
        true,
        Some(&name),
        true,
    )
    .map_err(|e| format!("保存状态失败：{e}"))?;
    let _ = app.log_event(student_id, "submit", &name);
    #[cfg(test)]
    seal_hook::notify(false);
    Ok(name)
}

/// 一次状态写盘的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PersistOutcome {
    Written,
    /// 期间已被交卷抢占：放弃这次「未交卷」快照。
    SkippedSubmitted,
}

/// 待冻结学生的数据快照（交卷接口与自动收卷共用）。
///
/// 克隆代价是卷面 + 作答（通常几十到几百 KB，上限 16 MiB）；换来的是加密与写盘
/// 全部在锁外进行。拷的是**值**而不是引用，因为锁一放，`Student` 可能被另一条路径改、
/// 被重新登录顶掉、或被当作长时间无活动清掉。
struct FreezeSnapshot {
    token: String,
    student_id: u64,
    session_id: u32,
    paper: picker::Paper,
    answers: BTreeMap<String, String>,
}

impl App {
    fn state_path(&self, student_id: u64, session_id: u32) -> PathBuf {
        self.state_dir
            .join(format!("{student_id}-{session_id}.json"))
    }

    fn load_state(
        &self,
        student_id: u64,
        session_id: u32,
    ) -> Option<(BTreeMap<String, String>, bool, Option<String>)> {
        let raw = std::fs::read(self.state_path(student_id, session_id)).ok()?;
        let plain = crypto::decrypt(&self.state_key, &raw, b"web-state").ok()?;
        let v: Value = serde_json::from_slice(&plain).ok()?;
        let answers = v
            .get("answers")?
            .as_object()?
            .iter()
            .filter_map(|(k, val)| Some((k.clone(), val.as_str()?.to_string())))
            .collect();
        let submitted = v.get("submitted").and_then(Value::as_bool).unwrap_or(false);
        let exported = v
            .get("exported")
            .and_then(Value::as_str)
            .map(str::to_string);
        Some((answers, submitted, exported))
    }

    /// 串行化写状态：先拿写锁，再按需短复查内存里的 `submitted`。
    ///
    /// 「保存」与「交卷」都可能在锁外写盘。若保存的「未交卷」快照后落地，就会把
    /// 交卷写下的「已交卷」覆盖回去（重启后学生回到可编辑状态、已冻结的卡被改写）。
    /// 这里用一把写盘锁串行化，并在写前复查：只要内存里已被交卷，就丢弃这次快照。
    #[allow(clippy::too_many_arguments)]
    fn persist_state(
        &self,
        token: Option<&str>,
        student_id: u64,
        session_id: u32,
        answers: &BTreeMap<String, String>,
        submitted: bool,
        exported: Option<&str>,
        durable: bool,
    ) -> std::io::Result<PersistOutcome> {
        // 只锁「这个学生」的写盘：50 人同时交卷时，durable fsync 不会把别人的保存排在后面。
        let student_lock = self.write_lock_for(student_id, session_id);
        let _write_guard = student_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !submitted && let Some(token) = token {
            let state = self.lock_state();
            if state
                .students
                .get(token)
                .is_some_and(|student| student.submitted)
            {
                return Ok(PersistOutcome::SkippedSubmitted);
            }
        }
        self.write_state(
            student_id, session_id, answers, submitted, exported, durable,
        )?;
        Ok(PersistOutcome::Written)
    }

    /// 取某个学生的写盘锁（按 `(学号, 场次)` 分桶；桶数量与学生数同阶）。
    ///
    /// 锁序：`write_locks`（极短）→ 学生锁 → `state`；没有反向路径。
    fn write_lock_for(&self, student_id: u64, session_id: u32) -> Arc<Mutex<()>> {
        let mut locks = self
            .write_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Arc::clone(
            locks
                .entry((student_id, session_id))
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    }

    /// 把作答状态写盘（加密 + 原子替换 + 收紧权限）。
    ///
    /// 参数是**数据**而不是 `&Student`：交卷、自动收卷与每次保存都要在锁外写状态，
    /// 拿不到 `&mut Student`；传引用字段又能避免为了写盘把整份作答再克隆一次。
    ///
    /// `durable`：只有交卷（含自动收卷）需要 `fsync`——那一份写不下去就等于学生丢卡；
    /// 高频的「保存一题」用 `false`，否则每 600 ms 一次 fsync 会把教师机磁盘打满。
    fn write_state(
        &self,
        student_id: u64,
        session_id: u32,
        answers: &BTreeMap<String, String>,
        submitted: bool,
        exported: Option<&str>,
        durable: bool,
    ) -> std::io::Result<()> {
        let payload = json!({
            "student_id": student_id,
            "session_id": session_id,
            "answers": answers,
            "submitted": submitted,
            "exported": exported,
        });
        let path = self.state_path(student_id, session_id);
        // 序号让并发写的临时名互不相同（历史上两个写者共用一个 `.tmp`，
        // 后 rename 的那个会因源文件已被移走而失败 → 学生偶发 500）。
        let seq = self
            .tmp_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = path.with_extension(format!("json.{seq}.tmp"));
        let plain = payload.to_string().into_bytes();
        let sealed = crypto::encrypt(&self.state_key, &plain, b"web-state")
            .map_err(|e| std::io::Error::other(format!("加密状态失败：{e}")))?;
        let result = (|| -> std::io::Result<()> {
            {
                use std::io::Write as _;
                // 必须用写句柄：只读句柄的 sync_all 在 Windows 上是 ERROR_ACCESS_DENIED，
                // 等于根本没刷盘（v1.4.5 的 durable 就是这么空转的）。
                let mut f = std::fs::File::create(&tmp)?;
                f.write_all(&sealed)?;
                if durable {
                    f.sync_all()?;
                }
            }
            // 先收紧临时文件、再改名：rename 保留文件自身的 ACL，目标文件从出现的
            // 第一刻起就是「仅当前用户」。目录 DACL 的继承本来也管用，这里是双保险。
            if let Err(e) = secure_fs::harden_file(&tmp) {
                // 内容已经加密；权限收紧失败只告警，不阻断落盘（否则学生会因此丢卡）。
                eprintln!("⚠ 状态文件权限收紧失败 {}：{e}", tmp.display());
            }
            std::fs::rename(&tmp, &path)?;
            if durable {
                // rename 的目录项也要落盘，否则断电后可能「卡在、状态回退」。
                fsync_dir(path.parent());
            }
            Ok(())
        })();
        if result.is_err() {
            // 失败路径不能留下未收紧的加密临时文件（磁盘满 / 目录只读时最典型）。
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }

    fn log_event(&self, student_id: u64, kind: &str, detail: &str) -> std::io::Result<()> {
        use std::io::Write as _;
        // 先拿写锁：事件日志由交卷 / 保存等多条线程并发追加，没有锁时一次 `writeln!`
        // 拆出的两次 `write` 会与别人的写入在字节层面交错，整行变成无法解析的乱码
        // （v1.4.5 的 12 人并发演练实测：12 条交卷只解析出 2 条）。
        let _guard = self
            .event_log
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let path = self.answers_dir.join("events.jsonl");
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        // 事件日志含学号与交卷时间，同样只留当前用户可读；失败只告警。
        if let Err(e) = secure_fs::harden_file(&path) {
            eprintln!("⚠ 事件日志权限收紧失败 {}：{e}", path.display());
        }
        // 自己拼好整行、一次 `write_all` 落盘：即使将来有人去掉这把锁，单次写也比
        // `writeln!` 的两段写安全得多。
        let line = format!(
            "{}\n",
            json!({"at": self.now(), "student_id": student_id, "kind": kind, "detail": detail})
        );
        f.write_all(line.as_bytes())
    }
}

fn time_info(app: &App, req: &Request) -> Response {
    let Some(token) = cookie_token(req) else {
        return Response::error(401, "未登录");
    };
    let state = app.lock_state();
    let Some(st) = state.students.get(&token) else {
        return Response::error(401, "登录已失效");
    };
    let (start, end) = session_window(app, st.session_id).unwrap_or((0, 0));
    // 前端每 15 秒轮询一次：这里顺手续期，长考试不会被 60 分钟 TTL 掐断。
    Response::json(
        200,
        json!({
            "server_time": app.now(),
            "start_unix": start,
            "end_unix": end,
            "remaining": end.saturating_sub(app.now()),
            // 前端每 15 s 轮询一次：让它能发现「服务端已经自动收卷了」，从而切到
            // 「已交卷」页，而不是继续在灰掉的输入框里打字、每次保存都吃 409。
            "submitted": st.submitted,
            // 诊断用：前端不展示，但老师/排障时能用 curl 确认「服务端时间是校准过的」。
            "time_mode": app.time_mode.as_str(),
            "clock_offset_secs": app.clock.offset_secs(),
        }),
    )
    .with_cookie(session_cookie(app, &token))
}

/// 场次窗口是否开着：`[start, end)`，与客户端 `decide_session` 同口径。
///
/// 抽成纯函数是为了能直接断言边界（同一秒 now == end 必须算「已结束」）；
/// 否则只能靠 sleep 去卡那一秒，测试会飘。
fn window_open(now: u64, start: u64, end: u64) -> bool {
    now >= start && now < end
}

/// 周期任务：到点自动收卷 + 清理长时间无活动的 token。
///
/// **锁分段**（v1.4.4）：原来是一把锁从收集做到冻结，冻结一个人要 AES-GCM + 两次写盘
/// + 一次 ACL 调用，期间所有学生的请求都堵在锁上。现在：
/// 1. 锁外先把各场次的窗口解析好（`parse_header` 要解密 header，也很贵）；
/// 2. 持锁只做内存遍历 + 克隆快照 + 清理超时 token；
/// 3. 锁外做密封与写盘（`seal_and_export`）；
/// 4. 再短暂持锁回填 `submitted` / `exported`。
fn sweep_tick(app: &App) {
    let now = app.now();
    // 窗口在启动时缓存好了（`App::windows`），这里只读，不再解密 header。
    let windows = &app.windows;
    // 退避状态放在 App 级：学生重新登录会新建 Student，放在 Student 上会归零。
    let retry: HashMap<u64, (u32, u64)> = app
        .finalize_retry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();

    // 第一阶段：持锁只做内存操作。
    let mut pending: Vec<FreezeSnapshot> = Vec::new();
    let mut stale: Vec<String> = Vec::new();
    {
        let mut state = app.lock_state();
        for (token, st) in state.students.iter() {
            let Some(&(_, end)) = windows.get(&st.session_id) else {
                continue;
            };
            let retry_at = retry.get(&st.student_id).map_or(0, |&(_, at)| at);
            // 窗口与客户端同口径：[start, end)。失败过的学生按退避时间跳过。
            if !window_open(now, 0, end) && !st.submitted && now >= retry_at {
                pending.push(FreezeSnapshot {
                    token: token.clone(),
                    student_id: st.student_id,
                    session_id: st.session_id,
                    paper: st.paper.clone(),
                    answers: st.answers.clone(),
                });
            }
            if now.saturating_sub(st.last_seen) > 7200 {
                stale.push(token.clone());
            }
        }

        // 抢占：先在锁内置 `submitted`，避免「学生点交卷」与「自动收卷」同时冻结同一人
        // （会写出两份同名校验一致的 .eea、事件日志重复两条，状态还会来回抖）。
        // 锁外冻结失败时再撤回（见下），下一轮 sweep 会重试。
        for snap in &pending {
            if let Some(st) = state.students.get_mut(&snap.token) {
                st.submitted = true;
            }
        }

        // 待冻结的学生不能当「无活动」清掉：清掉就没人回填 `exported` 了，
        // 虽然答题卡已经写盘，但 `/api/paper` 会显示「登录已失效」而不是「已交卷」。
        let pending_tokens: std::collections::HashSet<&str> =
            pending.iter().map(|s| s.token.as_str()).collect();
        for token in stale {
            if pending_tokens.contains(token.as_str()) {
                continue;
            }
            if let Some(st) = state.students.remove(&token) {
                state.tokens_by_student.remove(&st.student_id);
            }
        }
    }

    // 第二阶段：锁外加密与写盘。
    for snap in pending {
        let result = seal_and_export(
            app,
            snap.student_id,
            snap.session_id,
            &snap.paper,
            &snap.answers,
            now,
        );
        let mut state = app.lock_state();
        let Some(st) = state.students.get_mut(&snap.token) else {
            // 锁外被清理或被重新登录顶掉：答题卡与事件已经落盘，不必再回填内存。
            continue;
        };
        match result {
            Ok(name) => {
                st.exported = Some(name);
                st.last_seen = now;
                let mut retry = app
                    .finalize_retry
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                retry.remove(&snap.student_id);
            }
            Err(e) => {
                // 撤回抢占，让下一轮重试；否则这个学生会一直显示「已交卷」却没有答题卡。
                st.submitted = false;
                let mut retry = app
                    .finalize_retry
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let failures = retry.entry(snap.student_id).or_insert((0, 0));
                failures.0 = failures.0.saturating_add(1);
                failures.1 = now.saturating_add(backoff_secs(failures.0));
                eprintln!(
                    "自动收卷失败（学号 {}，场次 {}，第 {} 次，{} 秒后重试）：{e}",
                    snap.student_id,
                    snap.session_id,
                    failures.0,
                    backoff_secs(failures.0)
                );
            }
        }
    }
}

/// 一次请求读取的结果。
///
/// 显式区分「太胖」「分块传输」「对端关闭」，是为了能给出**对用户有用**的状态码：
/// 以前这些都变成 `Err`，连接被直接关掉，客户端只看到连接重置。
enum ReadOutcome {
    Request(Request),
    /// 声明长度超过该接口的上限（不读 body，直接 413）。
    Oversized(usize),
    /// `Transfer-Encoding: chunked`（暂不支持，411 而不是误导性的 400）。
    Chunked,
    /// 对端在发请求前关闭。
    Closed,
}

fn read_request(stream: &mut impl Read, peer: IpAddr) -> Result<ReadOutcome> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(ReadOutcome::Closed);
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_head_end(&buf) {
            break pos;
        }
        anyhow::ensure!(buf.len() < 64 * 1024, "请求头过大");
    };
    let head = std::str::from_utf8(&buf[..head_end]).context("请求头不是 UTF-8")?;
    let mut lines = head.split("\r\n");
    let first = lines.next().context("空请求")?;
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts
        .next()
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    if headers
        .get("transfer-encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
    {
        return Ok(ReadOutcome::Chunked);
    }
    let len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let limit = max_body_for(&path);
    if len > limit {
        // 先判后读：不能为了回一个 413 先把 body 读进内存。
        return Ok(ReadOutcome::Oversized(limit));
    }
    let body_start = head_end + 4;
    while buf.len() < body_start + len {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = buf[body_start..(body_start + len).min(buf.len())].to_vec();
    Ok(ReadOutcome::Request(Request {
        method,
        path,
        headers,
        body,
        peer,
    }))
}

/// 把没读掉的请求体短暂读掉，避免 `close` 发 RST 让客户端看不到 413/411。
///
/// 只在上限/分块分支调用；调用方用 `with_short_read_timeout` 把超时压到 200 ms，
/// 最多读 1 MiB，绝不无限等价于读完整 body。
fn drain_briefly(stream: &mut impl Read) {
    let mut buf = [0u8; 8192];
    let mut total = 0usize;
    while total < 1024 * 1024 {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => total += n,
        }
    }
}

/// 请求的真实来源 IP。
///
/// 必须是 TCP 对端地址——**不能**用 `X-Forwarded-For`（可伪造，等于让攻击者自选限速桶）。
/// v1.4.5 及以前这里被硬编码成 `127.0.0.1`：全场所有学生共用一个登录限速桶，
/// 第 21 个学生起就集体 429（40 人演练直接暴露）。
fn peer_ip(stream: &TcpStream) -> IpAddr {
    stream
        .peer_addr()
        .map(|addr| addr.ip())
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn write_response(stream: &mut impl Write, resp: Response) -> Result<()> {
    // 安全响应头对**所有**响应都加（含 4xx/5xx）：页面没有第三方资源，
    // 所以 CSP 可以收到最紧（`default-src 'none'` + 只放行同源脚本/样式）。
    // 这些头不防已经进了教师机的恶意软件，但能兜住「将来某个 sink 写错」这类 XSS，
    // 并阻止同网段页面用 iframe 做点击劫持。
    let mut out = format!(
        "HTTP/1.1 {} {}\r\n\
         Content-Type: {}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         Cache-Control: no-store\r\n\
         Content-Security-Policy: {}\r\n\
         X-Content-Type-Options: nosniff\r\n\
         X-Frame-Options: DENY\r\n\
         Referrer-Policy: no-referrer\r\n\
         Permissions-Policy: geolocation=(), camera=(), microphone=()\r\n",
        resp.status,
        status_text(resp.status),
        resp.content_type,
        resp.body.len(),
        CONTENT_SECURITY_POLICY
    );
    for (k, v) in &resp.headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    stream.write_all(out.as_bytes())?;
    stream.write_all(&resp.body)?;
    stream.flush()?;
    Ok(())
}

fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "OK",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_parse_ignores_other_cookies() {
        let req = Request {
            method: "GET".into(),
            path: "/".into(),
            headers: HashMap::from([(
                "cookie".to_string(),
                "a=1; ees_token=deadbeef; b=2".to_string(),
            )]),
            body: Vec::new(),
            peer: IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        };
        assert_eq!(cookie_token(&req).as_deref(), Some("deadbeef"));
    }

    #[test]
    fn head_end_finds_blank_line() {
        // 20 = 请求行 14 + CRLF 2 + "A: b" 4，之后就是 \r\n\r\n。
        assert_eq!(
            find_head_end(b"GET / HTTP/1.1\r\nA: b\r\n\r\nbody"),
            Some(20)
        );
        assert_eq!(find_head_end(b"GET / HTTP/1.1\r\n"), None);
    }
}
