//! 网页版学生端（阶段 0 原型）。
//!
//! 设计取舍：
//! - **只从已构建的考试包启动**：读 payload + K_manifest，不持有 master、不持有答案；
//! - 登录即校验：复用 `container::try_unlock_session_with_pin`，学号/PIN 错就没有信封；
//! - 每人只生成自己的试卷（`picker`），接口不会下发“整库”；
//! - 作答先放内存，交卷时用 `answer_sheet::seal` 写出与离线版同格式的 `.eea`。
//!
//! 这是**原型**，不是生产实现：没有 SQLite、没有 HTTPS、没有管理端 UI，
//! 登录限速也很粗糙。正式版本会换成 axum/tokio + SQLite + 管理端入口。

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use common::answer_sheet::{encode_fixes, encode_parts};
use common::{answer_sheet, container, crypto, picker, timegate};
use rand::RngCore;
use serde_json::{Value, json};

const INDEX_HTML: &str = include_str!("index.html");
const MAX_BODY: usize = 20 * 1024 * 1024;

mod clock;
mod secure_fs;

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

#[derive(Parser)]
#[command(
    name = "exam-server",
    version,
    about = "保密考试系统网页版（阶段 0 原型）"
)]
struct Cli {
    /// 已构建的考试包（exam-builder 产物）
    #[arg(long)]
    package: PathBuf,
    /// K_manifest（64 位 hex 文件，`exam-builder build --emit-keyfile` 生成）
    #[arg(long)]
    keyfile: PathBuf,
    /// 监听地址
    #[arg(long, default_value = "0.0.0.0")]
    bind: String,
    /// 监听端口
    #[arg(long, default_value_t = 8080)]
    port: u16,
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
    conns: std::sync::atomic::AtomicUsize,
    gate: Gate,
    state: Mutex<State>,
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
    fn html(body: &'static str) -> Self {
        Self {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: body.as_bytes().to_vec(),
            headers: Vec::new(),
        }
    }
    fn with_cookie(mut self, cookie: String) -> Self {
        self.headers.push(("Set-Cookie".into(), cookie));
        self
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
    fn acquire(&self) -> std::sync::MutexGuard<'_, usize> {
        let mut n = self.slots.lock().unwrap();
        while *n >= self.max {
            n = self.cv.wait(n).unwrap();
        }
        *n += 1;
        n
    }
    fn release(&self, mut guard: std::sync::MutexGuard<'_, usize>) {
        *guard -= 1;
        self.cv.notify_one();
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

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

    let bytes = std::fs::read(&cli.package)
        .with_context(|| format!("读取考试包 {}", cli.package.display()))?;
    let payload = container::extract_payload(&bytes)
        .context("考试包中没有 payload（请用 exam-builder 生成）")?;

    let key_text = std::fs::read_to_string(&cli.keyfile)
        .with_context(|| format!("读取 keyfile {}", cli.keyfile.display()))?;
    let key_bytes = hex::decode(key_text.trim()).context("keyfile 不是合法 hex")?;
    anyhow::ensure!(key_bytes.len() == 32, "keyfile 必须是 64 位 hex（32 字节）");
    let mut k_manifest = [0u8; 32];
    k_manifest.copy_from_slice(&key_bytes);

    let (header, _) = container::parse_header(payload, &k_manifest).context("解析 header 失败")?;
    // 用校准后的时间挑场次：本机时钟偏差大时，挑中哪一场会跟着变。
    let now = server_clock.now();
    let active = header.sessions.iter().find(|s| {
        match (
            timegate::parse_rfc3339(&s.start),
            timegate::parse_rfc3339(&s.end),
        ) {
            (Some(a), Some(b)) => now >= a && now <= b,
            _ => false,
        }
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
        .unwrap_or_else(|| cli.answers_dir.join("state"));
    std::fs::create_dir_all(&state_dir)
        .with_context(|| format!("创建状态目录 {}", state_dir.display()))?;
    harden_state_dir(&state_dir);

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
        state: Mutex::new(State::default()),
    });

    let listener = bind_listener(&cli.bind, cli.port)?;
    // 告诉学生该访问哪个地址：绑 0.0.0.0 / [::] 时用本机局域网 IP（教师机可能有多个），
    // 绑了具体地址（例如只绑 LAN、或只绑 127.0.0.1 调试）就直接用那个地址——
    // 否则绑了回环还会打印一个学生根本连不上的局域网 URL。
    println!(
        "考试服务已启动：http://{}:{}/",
        bind_host(&cli.bind),
        cli.port
    );
    println!(
        "场次 #{session_id}；答题卡输出到 {}",
        cli.answers_dir.display()
    );
    println!("（阶段 0 原型：仅内存保存、无管理端界面；Ctrl+C 结束）");

    {
        let sweep = Arc::clone(&app);
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(15));
                sweep_tick(&sweep);
            }
        });
    }

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let app = Arc::clone(&app);
                std::thread::spawn(move || {
                    if let Err(e) = handle(app, stream) {
                        eprintln!("连接处理失败：{e:#}");
                    }
                });
            }
            Err(e) => eprintln!("accept 失败：{e}"),
        }
    }
    Ok(())
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
    let Some(req) = read_request(&mut stream)? else {
        return Ok(());
    };
    let resp = route(&app, &req);
    write_response(&mut stream, resp)
}

fn route(app: &App, req: &Request) -> Response {
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => Response::html(INDEX_HTML),
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
        ("GET", "/api/time") => time_info(app, req),
        _ => Response::error(404, "not found"),
    }
}

fn login(app: &App, req: &Request) -> Response {
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
        let mut state = app.state.lock().unwrap();
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
        let mut state = app.state.lock().unwrap();
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

    let slot = app.gate.acquire();
    let unlocked = app.unlock(student_id, pin.as_deref());
    app.gate.release(slot);

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
            let mut state = app.state.lock().unwrap();
            if let Some(old) = state.tokens_by_student.insert(student_id, token.clone()) {
                state.students.remove(&old);
            }
            state.students.insert(token.clone(), student);
            Response::json(200, json!({"ok": true})).with_cookie(format!(
                "ees_token={token}; Path=/; HttpOnly; SameSite=Strict"
            ))
        }
        Err(msg) => Response::error(401, msg),
    }
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
            let (Some(start), Some(end)) = (
                timegate::parse_rfc3339(&session.start),
                timegate::parse_rfc3339(&session.end),
            ) else {
                continue;
            };
            if now < start || now > end {
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

fn paper(app: &App, req: &Request) -> Response {
    let Some(token) = cookie_token(req) else {
        return Response::error(401, "未登录");
    };
    let mut state = app.state.lock().unwrap();
    let Some(st) = state.students.get_mut(&token) else {
        return Response::error(401, "登录已失效");
    };
    st.last_seen = app.now();
    let (start, end) = session_window(app, st.session_id);
    Response::json(
        200,
        json!({
            "student_id": st.student_id,
            "session_id": st.session_id,
            "server_time": app.now(),
            "start_unix": start,
            "end_unix": end,
            "submitted": st.submitted,
            "paper": st.paper,
            "answers": st.answers,
        }),
    )
}

/// 某场次的时间窗；场次不存在或包坏了返回 `(0, 0)`（调用方按「没有窗口」处理）。
fn session_window(app: &App, session_id: u32) -> (u64, u64) {
    session_windows(app)
        .get(&session_id)
        .copied()
        .unwrap_or((0, 0))
}

/// 一次解析出所有场次的时间窗。
///
/// 解析 header 要解密 + 反序列化，单个请求量级还好，但 `sweep_tick` 是按学生循环的：
/// 每个学生解一次 header 纯属浪费（50 人就是 50 次 AES-GCM），所以那里先解一次。
fn session_windows(app: &App) -> HashMap<u32, (u64, u64)> {
    let Ok((header, _)) = container::parse_header(&app.payload, &app.k_manifest) else {
        return HashMap::new();
    };
    header
        .sessions
        .iter()
        .map(|s| {
            (
                s.session_id,
                (
                    timegate::parse_rfc3339(&s.start).unwrap_or(0),
                    timegate::parse_rfc3339(&s.end).unwrap_or(0),
                ),
            )
        })
        .collect()
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
        let mut state = app.state.lock().unwrap();
        let Some(st) = state.students.get_mut(&token) else {
            return Response::error(401, "登录已失效");
        };
        if st.submitted {
            return Response::error(409, "已交卷，不能再修改");
        }
        let (_, end) = session_window(app, st.session_id);
        if end != 0 && app.now() > end {
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
        // 填空/改错在离线客户端是 JSON 编码；网页端按“每行一个答案”转换，
        // 否则收卷判分会把它们当空。
        let normalized = match item.qtype.as_str() {
            "fill" => {
                let parts: Vec<String> = value
                    .lines()
                    .map(|l| l.trim_end_matches('\r').to_string())
                    .collect();
                encode_parts(&parts)
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
                encode_fixes(&fixes)
            }
            _ => value.to_string(),
        };
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
    if let Err(e) = app.write_state(student_id, session_id, &answers, false, None, false) {
        // 内存里已经有了，但磁盘上没有：必须让学生知道，让他重试这一题。
        return Response::error(
            500,
            format!("答案已收下但状态写盘失败：{e}（请重试这一题）"),
        );
    }
    Response::json(200, json!({"ok": true}))
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
        let mut state = app.state.lock().unwrap();
        let Some(st) = state.students.get_mut(&token) else {
            return Response::error(401, "登录已失效");
        };
        if st.submitted {
            if let Some(name) = st.exported.clone() {
                if app.answers_dir.join(&name).is_file() {
                    return Response::json(200, json!({"ok": true, "already": true, "file": name}));
                }
                // 状态说已交卷，但答题卡文件不见了（被手工删掉、盘符换了、目录被清）。
                // 退回去重写一份：学生没有卡比重复写一份严重得多。
                st.exported = None;
                st.submitted = false;
            } else {
                return Response::json(200, json!({"ok": true, "already": true, "file": null}));
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
    let mut state = app.state.lock().unwrap();
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
        }
        Err(e) => {
            st.submitted = false; // 撤回抢占：让学生能再试一次
            Response::error(500, e)
        }
    }
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
    std::fs::write(app.answers_dir.join(&name), &bytes)
        .map_err(|e| format!("写入答题卡失败：{e}"))?;
    app.write_state(student_id, session_id, answers, true, Some(&name), true)
        .map_err(|e| format!("保存状态失败：{e}"))?;
    let _ = app.log_event(student_id, "submit", &name);
    #[cfg(test)]
    seal_hook::notify(false);
    Ok(name)
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
        let tmp = path.with_extension("json.tmp");
        let plain = payload.to_string().into_bytes();
        let sealed = crypto::encrypt(&self.state_key, &plain, b"web-state")
            .map_err(|e| std::io::Error::other(format!("加密状态失败：{e}")))?;
        std::fs::write(&tmp, sealed)?;
        // 先收紧临时文件、再改名：rename 保留文件自身的 ACL，目标文件从出现的
        // 第一刻起就是「仅当前用户」。目录 DACL 的继承本来也管用，这里是双保险
        // （覆盖目录被替换成继承位不同、或状态目录被 --state-dir 指到别处的情况）。
        if let Err(e) = secure_fs::harden_file(&tmp) {
            // 内容已经加密；权限收紧失败只告警，不阻断落盘（否则学生会因此丢卡）。
            eprintln!("⚠ 状态文件权限收紧失败 {}：{e}", tmp.display());
        }
        std::fs::rename(&tmp, &path)?;
        if durable {
            let _ = std::fs::File::open(&path).and_then(|f| f.sync_all());
        }
        Ok(())
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
    let state = app.state.lock().unwrap();
    let Some(st) = state.students.get(&token) else {
        return Response::error(401, "登录已失效");
    };
    let (start, end) = session_window(app, st.session_id);
    Response::json(
        200,
        json!({
            "server_time": app.now(),
            "start_unix": start,
            "end_unix": end,
            "remaining": end.saturating_sub(app.now()),
            // 诊断用：前端不展示，但老师/排障时能用 curl 确认「服务端时间是校准过的」。
            "time_mode": app.time_mode.as_str(),
            "clock_offset_secs": app.clock.offset_secs(),
        }),
    )
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
    // 场次窗口在进程生命周期内不变，锁外解析一次即可（每个学生都解析一次纯属浪费）。
    let windows = session_windows(app);

    // 第一阶段：持锁只做内存操作。
    let mut pending: Vec<FreezeSnapshot> = Vec::new();
    let mut stale: Vec<String> = Vec::new();
    {
        let mut state = app.state.lock().unwrap();
        for (token, st) in state.students.iter() {
            let end = windows.get(&st.session_id).map_or(0, |(_, end)| *end);
            if end != 0 && now > end && !st.submitted {
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
        let mut state = app.state.lock().unwrap();
        let Some(st) = state.students.get_mut(&snap.token) else {
            // 锁外被清理或被重新登录顶掉：答题卡与事件已经落盘，不必再回填内存。
            continue;
        };
        match result {
            Ok(name) => {
                st.exported = Some(name);
                st.last_seen = now;
            }
            Err(e) => {
                // 撤回抢占，让下一轮重试；否则这个学生会一直显示「已交卷」却没有答题卡。
                st.submitted = false;
                eprintln!(
                    "自动收卷失败（学号 {}，场次 {}）：{e}",
                    snap.student_id, snap.session_id
                );
            }
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Result<Option<Request>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(None);
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
    let len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    anyhow::ensure!(len <= MAX_BODY, "请求体过大");
    let body_start = head_end + 4;
    while buf.len() < body_start + len {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = buf[body_start..(body_start + len).min(buf.len())].to_vec();
    Ok(Some(Request {
        method,
        path,
        headers,
        body,
        peer: peer_ip(stream),
    }))
}

/// 请求的真实来源 IP。
///
/// 必须是 TCP 对端地址——**不能**用 `X-Forwarded-For`（可伪造，等于让攻击者自选限速桶）。
/// v1.4.5 及以前这里被硬编码成 `127.0.0.1`：于是全场所有学生共用一个登录限速桶，
/// 第 21 个学生起就集体 429（40 人演练直接暴露）；「按 IP 限速」形同虚设。
///
/// 取不到对端地址（连接已断）时退回本机地址，宁可少限制一次也不要 panic。
fn peer_ip(stream: &TcpStream) -> IpAddr {
    stream
        .peer_addr()
        .map(|addr| addr.ip())
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn write_response(stream: &mut TcpStream, resp: Response) -> Result<()> {
    let mut out = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n",
        resp.status,
        status_text(resp.status),
        resp.content_type,
        resp.body.len()
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
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
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
