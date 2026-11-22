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
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::Parser;
use common::{answer_sheet, container, picker, timegate};
use rand::RngCore;
use serde_json::{Value, json};

const INDEX_HTML: &str = include_str!("index.html");
const MAX_BODY: usize = 20 * 1024 * 1024;

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
}

struct App {
    payload: Vec<u8>,
    k_manifest: [u8; 32],
    answers_dir: PathBuf,
    state_dir: PathBuf,
    forced_session: Option<u32>,
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

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn main() -> Result<()> {
    let cli = Cli::parse();

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
    let now = now_unix();
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

    let app = Arc::new(App {
        payload: payload.to_vec(),
        k_manifest,
        answers_dir: cli.answers_dir.clone(),
        state_dir,
        forced_session: cli.session.or(Some(session_id)),
        state: Mutex::new(State::default()),
    });

    let addr = format!("{}:{}", cli.bind, cli.port);
    let listener = TcpListener::bind(&addr).with_context(|| format!("监听 {addr} 失败"))?;
    println!("考试服务已启动：http://{}:{}/", local_ip(), cli.port);
    println!(
        "场次 #{session_id}；答题卡输出到 {}",
        cli.answers_dir.display()
    );
    println!("（阶段 0 原型：仅内存保存、无管理端界面；Ctrl+C 结束）");

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

fn handle(app: Arc<App>, mut stream: TcpStream) -> Result<()> {
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
    let ip = req
        .headers
        .get("x-forwarded-for")
        .and_then(|v| v.parse().ok())
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    {
        let mut state = app.state.lock().unwrap();
        let now = now_unix();
        let entry = state.attempts.entry(ip).or_insert((0, now));
        if now.saturating_sub(entry.1) > 60 {
            *entry = (0, now);
        }
        if entry.0 >= 20 {
            return Response::error(429, "登录尝试过多，请一分钟后再试");
        }
        entry.0 += 1;
    }

    let body: Value = match serde_json::from_slice(&req.body) {
        Ok(v) => v,
        Err(_) => return Response::error(400, "请求不是合法 JSON"),
    };
    let Some(student_id) = body.get("student_id").and_then(Value::as_u64) else {
        return Response::error(400, "缺少 student_id");
    };
    let pin = body.get("pin").and_then(Value::as_str).map(str::to_string);

    {
        let mut state = app.state.lock().unwrap();
        let now = now_unix();
        let entry = state.attempts_student.entry(student_id).or_insert((0, now));
        if now.saturating_sub(entry.1) > 60 {
            *entry = (0, now);
        }
        if entry.0 >= 10 {
            return Response::error(429, "该学号登录尝试过多，请一分钟后再试");
        }
        entry.0 += 1;
    }

    match app.unlock(student_id, pin.as_deref()) {
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
    fn unlock(&self, student_id: u64, pin: Option<&str>) -> Result<(String, Student), String> {
        let (header, blob) = container::parse_header(&self.payload, &self.k_manifest)
            .map_err(|e| format!("包解析失败：{e}"))?;
        let session_id = self
            .forced_session
            .ok_or_else(|| "未指定场次".to_string())?;
        let session = header
            .sessions
            .iter()
            .find(|s| s.session_id == session_id)
            .ok_or_else(|| "场次不存在".to_string())?;

        let now = now_unix();
        let start = timegate::parse_rfc3339(&session.start)
            .ok_or_else(|| "场次开始时间非法".to_string())?;
        let end =
            timegate::parse_rfc3339(&session.end).ok_or_else(|| "场次结束时间非法".to_string())?;
        if now < start {
            return Err("考试尚未开始".into());
        }
        if now > end {
            return Err("考试已结束".into());
        }

        let salt = hex::decode(&header.salt).map_err(|e| format!("salt 解码失败：{e}"))?;
        let unlocked =
            container::try_unlock_session_with_pin(session, &header, blob, student_id, &salt, pin)
                .map_err(|e| format!("解锁失败：{e}"))?
                .ok_or_else(|| "学号或 PIN 不正确".to_string())?;

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
        Ok((token, student))
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
    st.last_seen = now_unix();
    let (start, end) = session_window(app, st.session_id);
    Response::json(
        200,
        json!({
            "student_id": st.student_id,
            "session_id": st.session_id,
            "server_time": now_unix(),
            "start_unix": start,
            "end_unix": end,
            "submitted": st.submitted,
            "paper": st.paper,
            "answers": st.answers,
        }),
    )
}

fn session_window(app: &App, session_id: u32) -> (u64, u64) {
    let Ok((header, _)) = container::parse_header(&app.payload, &app.k_manifest) else {
        return (0, 0);
    };
    let Some(s) = header.sessions.iter().find(|s| s.session_id == session_id) else {
        return (0, 0);
    };
    (
        timegate::parse_rfc3339(&s.start).unwrap_or(0),
        timegate::parse_rfc3339(&s.end).unwrap_or(0),
    )
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

    let mut state = app.state.lock().unwrap();
    let Some(st) = state.students.get_mut(&token) else {
        return Response::error(401, "登录已失效");
    };
    if st.submitted {
        return Response::error(409, "已交卷，不能再修改");
    }
    let (_, end) = session_window(app, st.session_id);
    if end != 0 && now_unix() > end {
        return Response::error(409, "考试已结束");
    }
    let exists = st
        .paper
        .sections
        .iter()
        .flat_map(|s| &s.items)
        .any(|i| i.id == qid);
    if !exists {
        return Response::error(400, "题目不在本卷中");
    }
    st.answers.insert(qid.to_string(), value.to_string());
    let total: usize = st.answers.values().map(String::len).sum();
    if total > 16 * 1024 * 1024 {
        return Response::error(413, "整卡答案超过 16 MiB");
    }
    st.last_seen = now_unix();
    Response::json(200, json!({"ok": true}))
}

fn submit(app: &App, req: &Request) -> Response {
    if !csrf_ok(req) {
        return Response::error(403, "缺少 CSRF 头");
    }
    let Some(token) = cookie_token(req) else {
        return Response::error(401, "未登录");
    };
    let mut state = app.state.lock().unwrap();
    let Some(st) = state.students.get_mut(&token) else {
        return Response::error(401, "登录已失效");
    };
    if st.submitted {
        return Response::json(
            200,
            json!({"ok": true, "already": true, "file": st.exported}),
        );
    }
    match finalize(app, st) {
        Ok(name) => Response::json(200, json!({"ok": true, "file": name})),
        Err(e) => Response::error(500, e),
    }
}

/// 冻结并写出答题卡，同时把状态落盘；幂等。
fn finalize(app: &App, st: &mut Student) -> Result<String, String> {
    if let Some(name) = &st.exported {
        st.submitted = true;
        return Ok(name.clone());
    }
    let now = now_unix();
    let mut sheet =
        answer_sheet::AnswerSheet::from_paper(&st.paper, st.student_id, st.session_id, now);
    let qids: Vec<String> = st
        .paper
        .sections
        .iter()
        .flat_map(|s| s.items.iter().map(|i| i.id.clone()))
        .collect();
    for (idx, qid) in qids.iter().enumerate() {
        if let Some(v) = st.answers.get(qid) {
            sheet.set_answer(idx, Some(v.clone()), now);
        }
    }
    sheet.freeze(now);
    let bytes =
        answer_sheet::seal(&app.k_manifest, &sheet).map_err(|e| format!("生成答题卡失败：{e}"))?;
    let name = format!(
        "answers-{}-{}-{}.eea",
        st.student_id,
        st.session_id,
        &sheet.paper_fingerprint[..8]
    );
    std::fs::write(app.answers_dir.join(&name), &bytes)
        .map_err(|e| format!("写入答题卡失败：{e}"))?;
    st.submitted = true;
    st.exported = Some(name.clone());
    st.last_seen = now;
    app.save_state(st)
        .map_err(|e| format!("保存状态失败：{e}"))?;
    let _ = app.log_event(st.student_id, "submit", &name);
    Ok(name)
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
        let text = std::fs::read_to_string(self.state_path(student_id, session_id)).ok()?;
        let v: Value = serde_json::from_str(&text).ok()?;
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

    fn save_state(&self, st: &Student) -> std::io::Result<()> {
        let payload = json!({
            "student_id": st.student_id,
            "session_id": st.session_id,
            "answers": st.answers,
            "submitted": st.submitted,
            "exported": st.exported,
        });
        let path = self.state_path(st.student_id, st.session_id);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, payload.to_string())?;
        std::fs::rename(&tmp, &path)?;
        let _ = std::fs::File::open(&path).and_then(|f| f.sync_all());
        Ok(())
    }

    fn log_event(&self, student_id: u64, kind: &str, detail: &str) -> std::io::Result<()> {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.answers_dir.join("events.jsonl"))?;
        writeln!(
            f,
            "{}",
            json!({"at": now_unix(), "student_id": student_id, "kind": kind, "detail": detail})
        )
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
            "server_time": now_unix(),
            "start_unix": start,
            "end_unix": end,
            "remaining": end.saturating_sub(now_unix()),
        }),
    )
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
    }))
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
