//! exam-server 的失败分支集成测试（第 3 项，P2）。
//!
//! 这些用例直接调 `route()`，不经过 TCP：既覆盖状态码与 JSON 字段，也不需要拉起
//! 真实端口；只有「端口占用」一条走 `bind_listener`。
//!
//! 为什么值得写：happy path 已经由 `scripts/smoke-web.sh` 端到端覆盖，但**失败分支**
//! 一直是空白——错误 PIN、限速、状态文件损坏、非法请求体这些路径恰恰是现场最容易
//! 出事的地方：状态文件坏掉时若返回 500，学生就再也进不去考场；写接口缺 CSRF 校验
//! 则任何网页都能替学生改答案。这里的每条断言都盯着「用户看得见的东西」：
//! 状态码、`error` 文案、磁盘上的 `.eea` / 状态文件 / 事件日志。
//!
//! 每个用例自带 fixture（独立目录 + `Drop` 清理），互不依赖执行顺序。

use std::path::Path;
use std::sync::MutexGuard;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use common::crypto::KdfParams;
use common::model::{ExamBank, Question, QuestionBody, Session, SessionExam};
use common::timegate;

use super::*;

const STUDENT: u64 = 2021001;
const SESSION_ID: u32 = 1;
const PIN: &str = "123456";
const BANK_FILE: &str = "demo";
const MASTER: [u8; 32] = [0x5a; 32];
const SALT: [u8; 16] = [0x11; 16];
/// 最小 Argon2 参数：真实参数（64 MiB / t=3）单次约 92 ms，几十次登录会让测试变得很慢，
/// 而这里要验证的是**限速与校验逻辑**，与 KDF 强度无关。
const KDF: KdfParams = KdfParams {
    m_cost: 8,
    t_cost: 1,
    p_cost: 1,
};

/// 全局串行锁。
///
/// `seal_hook` 是进程级观察点（main.rs 为了让「密封不在锁内」可确定性验证而提供），
/// 用例并行跑会互相串台；Windows 上临时目录与 ACL 操作也更容易互相踩。
/// 用例本身都很快，串行换确定性是划算的。
static SERIAL: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// fixture
// ---------------------------------------------------------------------------

struct Fixture {
    app: Arc<App>,
    dir: PathBuf,
    banks: Vec<ExamBank>,
    pins: Option<container::PinMap>,
    /// 持有到用例结束：保证同一时刻只有一个用例在跑（见 `SERIAL`）。
    _serial: MutexGuard<'static, ()>,
}

impl Fixture {
    /// 3 道题（单选 / 填空 / 代码各一），无 PIN。
    fn new(tag: &str) -> Self {
        Self::with_questions(tag, 3, None)
    }

    /// 启用了每生 PIN 的包。
    fn with_pins(tag: &str) -> Self {
        Self::with_questions(tag, 3, Some(PIN.to_string()))
    }

    fn with_questions(tag: &str, count: usize, pin: Option<String>) -> Self {
        let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir = test_dir(tag);
        let answers_dir = dir.join("answers");
        let state_dir = answers_dir.join("state");
        std::fs::create_dir_all(&state_dir).expect("创建测试目录失败");

        let banks = vec![demo_bank(count)];
        let pins = pin.map(|p| {
            let mut map = container::PinMap::new();
            map.insert((SESSION_ID, STUDENT), p);
            map
        });

        let now = unix_now();
        let payload = build_payload(&banks, &pins, now.saturating_sub(60), now + 3600);
        let k_manifest = container::manifest_key(&MASTER);
        let state_key = *crypto::hkdf_derive(&k_manifest, b"web-state", "web-state");
        let (header, _) = container::parse_header(&payload, &k_manifest).expect("解析测试 payload");
        let windows = session_windows_from(&header).expect("测试 payload 的场次时间必须带时区");

        let app = Arc::new(App {
            payload,
            k_manifest,
            answers_dir,
            state_dir,
            forced_session: None,
            state_key,
            clock: clock::Clock::system(),
            time_mode: clock::TimeMode::Off,
            conns: std::sync::atomic::AtomicUsize::new(0),
            gate: Gate::new(4),
            event_log: Mutex::new(()),
            write_locks: Mutex::new(HashMap::new()),
            tmp_seq: std::sync::atomic::AtomicU64::new(0),
            // 真实默认是 60；这里收窄到 20，让「第 21 次 429」的用例短一点。
            login_rate_limit: 20,
            state: Mutex::new(State::default()),
            windows,
            finalize_retry: Mutex::new(HashMap::new()),
            // 默认按「仅教师可查看原卷」——与老师默认打包一致。
            review: common::model::StudentReviewPolicy::None,
            // 单元测试直接调 route()，不经过监听层，所以默认是明文模式。
            tls: None,
            secure_cookies: false,
            monitor_token: "a".repeat(64),
            monitor_write_mode: monitor::MonitorWriteMode::Allow,
            // 测试里默认关掉写冷却：不然同一个用例里连着两次写操作会互相 429。
            // 单独有一条用例把它调回 1 来验证限速本身。
            monitor_write_cooldown_secs: 0,
            monitor_last_write: Mutex::new(None),
            monitor_weak_token: false,
            roster: HashSet::new(),
            roster_known: false,
            roster_len: 0,
        });

        Self {
            app,
            dir,
            banks,
            pins,
            _serial: serial,
        }
    }

    fn route(&self, req: &Request) -> Response {
        route(&self.app, req)
    }

    /// 模拟「进程重启」：内存状态清零、换一个全新的 `App`，但磁盘上的答题卡与状态
    /// 文件原样保留。用来验证「教师机崩溃 / 断电 / 误杀进程后重登能否拿回作答」。
    fn restart(&mut self) {
        let old = Arc::clone(&self.app);
        self.app = Arc::new(App {
            payload: old.payload.clone(),
            k_manifest: old.k_manifest,
            answers_dir: old.answers_dir.clone(),
            state_dir: old.state_dir.clone(),
            forced_session: old.forced_session,
            state_key: old.state_key,
            clock: clock::Clock::system(),
            time_mode: clock::TimeMode::Off,
            conns: std::sync::atomic::AtomicUsize::new(0),
            gate: Gate::new(4),
            event_log: Mutex::new(()),
            write_locks: Mutex::new(HashMap::new()),
            tmp_seq: std::sync::atomic::AtomicU64::new(0),
            login_rate_limit: old.login_rate_limit,
            state: Mutex::new(State::default()),
            windows: old.windows.clone(),
            finalize_retry: Mutex::new(HashMap::new()),
            review: old.review,
            tls: old.tls.clone(),
            secure_cookies: old.secure_cookies,
            monitor_token: old.monitor_token.clone(),
            monitor_write_mode: old.monitor_write_mode,
            monitor_write_cooldown_secs: old.monitor_write_cooldown_secs,
            monitor_last_write: Mutex::new(None),
            monitor_weak_token: old.monitor_weak_token,
            roster: old.roster.clone(),
            roster_known: old.roster_known,
            roster_len: old.roster_len,
        });
    }

    fn login_from(&self, peer: IpAddr, student: u64, pin: Option<&str>) -> Response {
        let body = match pin {
            Some(p) => json!({"student_id": student, "pin": p}),
            None => json!({"student_id": student}),
        };
        self.route(&post_json("/api/login", body, peer).csrf())
    }

    fn login(&self, student: u64) -> Response {
        self.login_from(peer(1), student, None)
    }

    /// 登录并返回 token（断言 200）。
    fn login_ok(&self, student: u64) -> String {
        let resp = self.login(student);
        assert_status(&resp, 200, "名单内学号应登录成功");
        token_from_cookie(&resp)
    }

    fn paper_json(&self, token: &str) -> Value {
        let resp = self.route(&get("/api/paper", peer(1)).token(token));
        assert_status(&resp, 200, "取卷应成功");
        json_body(&resp)
    }

    fn qids(&self, token: &str) -> Vec<String> {
        self.paper_json(token)["paper"]["sections"][0]["items"]
            .as_array()
            .unwrap_or_else(|| panic!("试卷里应有 sections[0].items"))
            .iter()
            .map(|item| {
                item["id"]
                    .as_str()
                    .unwrap_or_else(|| panic!("题目缺少 id：{item}"))
                    .to_string()
            })
            .collect()
    }

    fn save(&self, token: &str, qid: &str, value: &str) -> Response {
        self.route(
            &put_json("/api/answer", json!({"qid": qid, "value": value}), peer(1))
                .csrf()
                .token(token),
        )
    }

    fn submit(&self, token: &str) -> Response {
        self.route(
            &post_json("/api/submit", json!({}), peer(1))
                .csrf()
                .token(token),
        )
    }

    /// 直接往内存里的作答表塞一题（不走 HTTP）。
    ///
    /// 只给「整卡超限」这类**需要先有 16 MiB 卡片**的用例用：走 HTTP 逐题填满会写 16 次
    /// 状态文件（合计 136 MB），在 Windows 上被杀毒扫描拖到 27 秒；这里只造内存态，
    /// 真正被测的 413 判定与回滚仍走一次真实请求。
    fn seed_answer(&self, token: &str, qid: &str, value: &str) {
        let mut state = self.app.state.lock().unwrap();
        let st = state.students.get_mut(token).expect("token 应有效");
        st.answers.insert(qid.to_string(), value.to_string());
    }

    /// 往状态表里直接插一个学生（只给「批量收卷部分失败」这类需要在场多人的用例用）。
    ///
    /// 为什么不再登录一次：同学号是单会话（重新登录会顶掉旧 token），造不出
    /// 「两个人同时在线」的场面；这里借用已有学生的卷面，复制出第二个学号。
    fn insert_fake_student(&self, token: &str, student_id: u64) {
        let now = self.app.now();
        let mut state = self.app.lock_state();
        let (paper, session_id) = {
            let source = state
                .students
                .values()
                .next()
                .expect("先登录一个学生，借用他的卷面");
            (source.paper.clone(), source.session_id)
        };
        state.students.insert(
            token.to_string(),
            Student {
                student_id,
                session_id,
                paper,
                answers: BTreeMap::new(),
                submitted: false,
                exported: None,
                last_seen: now,
                logged_in_at: now,
                submitted_at: 0,
            },
        );
        state
            .tokens_by_student
            .insert(student_id, token.to_string());
    }

    fn state_path(&self) -> PathBuf {
        self.app
            .state_dir
            .join(format!("{STUDENT}-{SESSION_ID}.json"))
    }

    fn eea_files(&self) -> Vec<PathBuf> {
        files_with_ext(&self.app.answers_dir, "eea")
    }

    /// 设置名单（`--roster` 的等价物）。
    fn set_roster(&mut self, ids: &[u64]) {
        let app = Arc::get_mut(&mut self.app).expect("换名单时不应有其它 Arc 持有者");
        app.roster = ids.iter().copied().collect();
        app.roster_len = ids.len();
        app.roster_known = true;
    }

    /// 切换监考写操作开关。
    fn set_monitor_write_mode(&mut self, mode: monitor::MonitorWriteMode) {
        let app = Arc::get_mut(&mut self.app).expect("换写模式时不应有其它 Arc 持有者");
        app.monitor_write_mode = mode;
    }

    /// 带监考令牌的 GET。
    fn monitor_get(&self, path: &str) -> Response {
        let mut req = get(path, peer(1));
        req.headers
            .insert("x-monitor-token".into(), self.app.monitor_token.clone());
        self.route(&req)
    }

    /// 带监考令牌 + CSRF 的 POST。
    fn monitor_post(&self, path: &str, body: Value) -> Response {
        let mut req = post_json(path, body, peer(1)).csrf();
        req.headers
            .insert("x-monitor-token".into(), self.app.monitor_token.clone());
        self.route(&req)
    }

    fn monitor_stats(&self) -> Value {
        let resp = self.monitor_get("/api/monitor/stats");
        assert_status(&resp, 200, "带令牌取监考统计应成功");
        json_body(&resp)
    }

    fn events(&self) -> Vec<Value> {
        let path = self.app.answers_dir.join("events.jsonl");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Vec::new();
        };
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("事件日志每行都应是 JSON"))
            .collect()
    }

    /// 给 fixture 装上 TLS 配置（`handle()` 会据此握手）；同时打开 Secure cookie。
    fn set_tls(&mut self, config: Arc<rustls::ServerConfig>) {
        let app = Arc::get_mut(&mut self.app).expect("装 TLS 时不应有其它 Arc 持有者");
        app.tls = Some(config);
        app.secure_cookies = true;
    }

    /// 切换「cookie 是否带 Secure」（TLS 模式为 true）。
    fn set_secure_cookies(&mut self, secure: bool) {
        let app = Arc::get_mut(&mut self.app).expect("换 cookie 策略时不应有其它 Arc 持有者");
        app.secure_cookies = secure;
    }

    /// 改包里的回看策略（网页端要按它决定交卷后还下不下发卷面）。
    fn set_review(&mut self, policy: common::model::StudentReviewPolicy) {
        let app = Arc::get_mut(&mut self.app).expect("换策略时不应有其它 Arc 持有者");
        app.review = policy;
    }

    /// 把场次窗口设成任意区间（重打包 payload + **同步刷新窗口缓存**）。
    fn set_window(&mut self, start: u64, end: u64) {
        let payload = build_payload(&self.banks, &self.pins, start, end);
        let app = Arc::get_mut(&mut self.app).expect("换 payload 时不应有其它 Arc 持有者");
        app.payload = payload;
        let (header, _) = container::parse_header(&app.payload, &app.k_manifest)
            .expect("解析新 payload 的 header");
        app.windows = session_windows_from(&header).expect("测试 payload 的场次时间必须带时区");
    }

    /// 把场次窗口改成「已经结束」，模拟时间走过收卷时刻。
    ///
    /// 为什么不 `sleep`：场次时间是**整秒**，`end = 现在 + 1` 再 `sleep(1.2s)` 能否跨过
    /// 边界取决于构造 fixture 时的小数部分（实测会在 0–0.8 秒之间抖），这种用例迟早
    /// 变成 CI 里的偶发红灯。直接换一份 `end` 已过去的 payload 是确定性的，也快。
    ///
    /// 只替换 `App::payload` + `App::windows`：已登录学生的内存试卷不受影响，
    /// 但窗口自 v1.4.9 起是**启动时缓存**的，不同步刷新缓存「过期」不会生效。
    fn expire_session(&mut self) {
        let now = unix_now();
        let payload = build_payload(
            &self.banks,
            &self.pins,
            now.saturating_sub(3600),
            now.saturating_sub(5),
        );
        let app = Arc::get_mut(&mut self.app).expect("换 payload 时不应有其它 Arc 持有者");
        app.payload = payload;
        let (header, _) = container::parse_header(&app.payload, &app.k_manifest)
            .expect("解析新 payload 的 header");
        app.windows = session_windows_from(&header).expect("测试 payload 的场次时间必须带时区");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// 卸掉全局观察器的 RAII 包装：用例 panic 时也要清掉 `seal_hook`，
/// 否则后续用例会一直触发这个闭包（它持有 `Arc<App>`，还会让 `Arc::get_mut` 失效）。
struct HookGuard;

impl Drop for HookGuard {
    fn drop(&mut self) {
        seal_hook::set(None);
    }
}

// ---------------------------------------------------------------------------
// 构造工具
// ---------------------------------------------------------------------------

fn test_dir(tag: &str) -> PathBuf {
    // AGENTS.md：测试禁止用 `std::env::temp_dir()`（MSYS 下原生进程写不进 /tmp）。
    // 统一放仓库 `target/` 下；目录名带进程 id，避免残留目录与并行用例互撞。
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("定位仓库根目录失败");
    let dir = root
        .join("target")
        .join("server-tests")
        .join(format!("{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Unix 秒 → RFC3339（显式 `+08:00`）。
///
/// 复用 `timegate` 的日历换算，避免测试里再手写一遍日期算法；时区**必须**显式带偏移
/// ——仓库不变量：无偏移时间会被当成 UTC，静默偏 8 小时。
fn rfc3339(unix: u64) -> String {
    let local = timegate::format_unix_at_offset(unix, 8 * 3600);
    format!("{}+08:00", local.replacen(' ', "T", 1))
}

fn session(start: u64, end: u64) -> Session {
    Session {
        session_id: SESSION_ID,
        start: rfc3339(start),
        end: rfc3339(end),
        students: vec![STUDENT],
        exams: vec![SessionExam {
            file: BANK_FILE.into(),
            pick: None,
            difficulty_min: None,
            difficulty_max: None,
        }],
        sections: Vec::new(),
    }
}

fn build_payload(
    banks: &[ExamBank],
    pins: &Option<container::PinMap>,
    start: u64,
    end: u64,
) -> Vec<u8> {
    let sessions = vec![session(start, end)];
    container::build_payload_with_pins(&MASTER, banks, &sessions, &SALT, KDF, false, pins.as_ref())
        .expect("构造 payload 失败（fixture 参数不合法）")
}

/// 测试题库：前两题固定为「单选 + 填空」，其余补单选（用于整卡容量上限那条用例）。
fn demo_bank(count: usize) -> ExamBank {
    let mut questions = Vec::with_capacity(count.max(3));
    for index in 0..count.max(3) {
        let (id, qtype, options) = match index {
            0 => (
                "q-single".to_string(),
                "single",
                vec!["1".into(), "2".into(), "3".into(), "4".into()],
            ),
            1 => ("q-fill".to_string(), "fill", Vec::new()),
            _ => (
                format!("q{index:02}"),
                "single",
                vec!["1".into(), "2".into(), "3".into(), "4".into()],
            ),
        };
        questions.push(Question {
            id: id.clone(),
            qtype: qtype.into(),
            stem: format!("第 {index} 题：1+1=?"),
            options,
            body: QuestionBody::Text,
            code_language: None,
            code_languages: Vec::new(),
            kp: None,
            difficulty: Some(1),
            answer: None,
            judge: None,
            score: 2,
        });
    }
    ExamBank {
        exam_id: BANK_FILE.into(),
        title: "测试题库".into(),
        pick: questions.len() as u32,
        questions,
    }
}

// ---------------------------------------------------------------------------
// 请求 / 响应工具
// ---------------------------------------------------------------------------

fn peer(index: u8) -> IpAddr {
    IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, index))
}

/// 取响应体 JSON（`Response` 只存字节，测试里统一从字节解析）。
fn body_json(resp: &Response) -> Value {
    serde_json::from_slice(&resp.body).unwrap_or_else(|e| {
        panic!(
            "响应不是 JSON：{e}，body={}",
            String::from_utf8_lossy(&resp.body)
        )
    })
}

fn request(method: &str, path: &str, peer: IpAddr, body: Vec<u8>) -> Request {
    Request {
        method: method.into(),
        path: path.into(),
        headers: HashMap::new(),
        body,
        peer,
    }
}

fn get(path: &str, peer: IpAddr) -> Request {
    request("GET", path, peer, Vec::new())
}

fn post_json(path: &str, body: Value, peer: IpAddr) -> Request {
    request("POST", path, peer, body.to_string().into_bytes())
}

fn put_json(path: &str, body: Value, peer: IpAddr) -> Request {
    request("PUT", path, peer, body.to_string().into_bytes())
}

impl Request {
    fn csrf(mut self) -> Self {
        self.headers.insert("x-ees-csrf".into(), "1".into());
        self
    }
    fn token(mut self, token: &str) -> Self {
        self.headers
            .insert("cookie".into(), format!("ees_token={token}"));
        self
    }
}

fn json_body(resp: &Response) -> Value {
    serde_json::from_slice(&resp.body).unwrap_or_else(|e| {
        panic!(
            "响应不是合法 JSON（{e}）：status={} body={:?}",
            resp.status,
            String::from_utf8_lossy(&resp.body)
        )
    })
}

fn err_msg(resp: &Response) -> String {
    json_body(resp)
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn assert_status(resp: &Response, want: u16, what: &str) {
    assert_eq!(
        resp.status,
        want,
        "{what}：期望 {want}，实际 {} —— body={:?}",
        resp.status,
        String::from_utf8_lossy(&resp.body)
    );
}

fn token_from_cookie(resp: &Response) -> String {
    let raw = resp
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("Set-Cookie"))
        .unwrap_or_else(|| panic!("登录成功必须下发 Set-Cookie；headers={:?}", resp.headers))
        .1
        .clone();
    raw.split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .strip_prefix("ees_token=")
        .unwrap_or_else(|| panic!("Set-Cookie 里没有 ees_token：{raw}"))
        .to_string()
}

fn files_with_ext(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().and_then(|e| e.to_str()) == Some(ext))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

fn file_names(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 1. 登录：PIN 与身份错误
// ---------------------------------------------------------------------------

/// 启用 PIN 后，错误 PIN 与漏填 PIN 都必须被拒绝。
///
/// 为什么重要：PIN 是「同班同学互相替考」的唯一屏障；如果错误 PIN 也能拿到信封，
/// 这个开关就形同虚设，而界面上看不出任何异常。
#[test]
fn wrong_and_missing_pin_are_rejected() {
    let f = Fixture::with_pins("pin");

    let ok = f.login_from(peer(1), STUDENT, Some(PIN));
    assert_status(&ok, 200, "正确 PIN 应登录成功");
    assert_eq!(json_body(&ok)["ok"], json!(true));
    assert!(!token_from_cookie(&ok).is_empty(), "登录成功必须下发 token");

    let wrong = f.login_from(peer(1), STUDENT, Some("000000"));
    assert_status(&wrong, 401, "错误 PIN 必须被拒绝");
    assert!(
        err_msg(&wrong).contains("学号或 PIN"),
        "提示应说明是学号/PIN 不对，而不是暴露「这个学号存在」：{}",
        err_msg(&wrong)
    );

    let missing = f.login_from(peer(1), STUDENT, None);
    assert_status(&missing, 401, "启用 PIN 后缺 pin 字段必须被拒绝");
    assert!(
        err_msg(&missing).contains("学号或 PIN"),
        "实际：{}",
        err_msg(&missing)
    );
}

/// 名单外学号 401；缺 `student_id` / 请求体不是 JSON 则是 400（客户端 bug，不是身份失败）。
///
/// 为什么区分 400 与 401：把客户端格式错误报成「学号或 PIN 不正确」会让学生反复试 PIN，
/// 现场排障时也看不出真正原因。
#[test]
fn unknown_student_and_malformed_bodies_are_rejected() {
    let f = Fixture::new("bad-login");

    let stranger = f.login_from(peer(1), 9_999_999, None);
    assert_status(&stranger, 401, "名单外学号必须被拒绝");
    assert!(
        err_msg(&stranger).contains("学号或 PIN"),
        "实际：{}",
        err_msg(&stranger)
    );

    let no_id = f.route(&post_json("/api/login", json!({"pin": "1"}), peer(1)).csrf());
    assert_status(&no_id, 400, "缺 student_id 应是 400");
    assert!(
        err_msg(&no_id).contains("缺少 student_id"),
        "实际：{}",
        err_msg(&no_id)
    );

    let not_json = f.route(&request("POST", "/api/login", peer(1), b"{not json".to_vec()).csrf());
    assert_status(&not_json, 400, "请求体不是 JSON 应是 400");
    assert!(
        err_msg(&not_json).contains("不是合法 JSON"),
        "实际：{}",
        err_msg(&not_json)
    );
}

// ---------------------------------------------------------------------------
// 2. 登录限速（按 IP / 按学号 / 窗口过期）
// ---------------------------------------------------------------------------

/// 同一 IP 第 21 次登录必须 429，且换一个 IP 立刻可用。
///
/// 为什么重要：限速是挡「一台机器穷举学号」的唯一手段；而如果限速是全局的，
/// 机房里一个人试错就会连坐全班——所以要同时断言「按 IP 计」。
#[test]
fn ip_login_limit_blocks_the_21st_attempt() {
    let f = Fixture::new("ip-limit");

    // 用名单外学号：同一学号 10 次就会先撞到学号维度的限速，掩盖这条 IP 断言。
    for i in 0..20u64 {
        let resp = f.login_from(peer(9), 9_000_000 + i, None);
        assert_status(&resp, 401, "前 20 次应放行到身份校验");
    }
    let blocked = f.login_from(peer(9), 9_000_099, None);
    assert_status(&blocked, 429, "同一 IP 第 21 次必须被限速");
    assert!(
        err_msg(&blocked).contains("一分钟"),
        "提示要告诉用户等多久：{}",
        err_msg(&blocked)
    );

    let other = f.login_from(peer(8), STUDENT, None);
    assert_status(&other, 200, "另一个 IP 不应受影响");
}

/// 格式错误的请求不应消耗「按 IP」的登录配额。
///
/// 为什么重要：同一 NAT 后面全班共用一个 IP 桶。旧客户端、浏览器插件、扫描器
/// 发几个坏请求就把名额占满，真实学生会被 429 挡在门外一分钟——现场表现为
/// 「全班突然都登不上，过一会儿又好了」，极难排查。
#[test]
fn malformed_requests_do_not_consume_login_quota() {
    let f = Fixture::new("bad-json-quota");
    let limit = f.app.login_rate_limit as usize;

    for _ in 0..limit + 5 {
        let bad = f.route(&request("POST", "/api/login", peer(3), b"not json".to_vec()).csrf());
        assert_status(&bad, 400, "非法 JSON 应 400");
    }
    // 合法 JSON 但缺学号：同样不是一次「登录尝试」。
    for _ in 0..limit + 5 {
        let missing = f.route(&post_json("/api/login", json!({"pin": "1"}), peer(3)).csrf());
        assert_status(&missing, 400, "缺学号应 400");
    }

    assert_status(
        &f.login_from(peer(3), STUDENT, None),
        200,
        "坏请求不得占用登录配额",
    );
}

/// 同一学号（不同 IP）第 11 次必须 429。
///
/// 为什么重要：只按 IP 限速挡不住「换 IP 继续试同一个学号的 PIN」。
#[test]
fn student_login_limit_blocks_the_11th_attempt() {
    let f = Fixture::new("student-limit");

    for i in 0..10u8 {
        let resp = f.login_from(peer(20 + i), 8_888_888, None);
        assert_status(&resp, 401, "前 10 次应放行到身份校验（名单外 → 401）");
    }
    let blocked = f.login_from(peer(30), 8_888_888, None);
    assert_status(&blocked, 429, "同一学号第 11 次必须被限速");
    assert!(
        err_msg(&blocked).contains("该学号"),
        "实际：{}",
        err_msg(&blocked)
    );
}

/// 限速窗口过期后必须重新放行。
///
/// 为什么重要：限速写成「永久拉黑」的话，课上一次误操作就会让学生整场进不来。
/// 这里改内存里的时间戳（等价于一分钟过去），而不是 sleep 60 秒。
#[test]
fn ip_limit_window_resets_after_a_minute() {
    let f = Fixture::new("limit-reset");

    for i in 0..20u64 {
        assert_status(
            &f.login_from(peer(9), 9_100_000 + i, None),
            401,
            "填满一分钟窗口",
        );
    }
    assert_status(
        &f.login_from(peer(9), 9_100_099, None),
        429,
        "第 21 次应先被限速",
    );

    {
        let mut state = f.app.state.lock().unwrap();
        state
            .attempts
            .insert(peer(9), (20, unix_now().saturating_sub(120)));
    }

    let after = f.login_from(peer(9), STUDENT, None);
    assert_status(&after, 200, "窗口过期后应重新放行");
    assert_eq!(
        f.app.state.lock().unwrap().attempts[&peer(9)].0,
        1,
        "窗口过期后计数应重置、再从 1 开始"
    );
}

// ---------------------------------------------------------------------------
// 3. 状态文件：损坏 / 换密钥 / 密文
// ---------------------------------------------------------------------------

/// 状态文件损坏时仍能登录，且表现为「没有状态」而不是 500。
///
/// 为什么重要：断电、磁盘满、半截写入都会让状态文件解不开。如果这时登录直接 500，
/// 学生就被永久挡在考场外；正确行为是丢掉坏状态、让人重新作答（答题卡另外还有）。
#[test]
fn corrupted_state_file_still_lets_the_student_in() {
    let f = Fixture::new("corrupt-state");

    let token = f.login_ok(STUDENT);
    let qid = f.qids(&token)[0].clone();
    assert_status(&f.save(&token, &qid, "B"), 200, "保存应成功");
    assert_status(&f.submit(&token), 200, "交卷应成功");

    let path = f.state_path();
    assert!(path.is_file(), "交卷后应写出状态文件：{}", path.display());
    std::fs::write(&path, b"\x00\x01broken-state-file\xff").expect("写坏状态文件失败");

    let again = f.login_ok(STUDENT);
    let paper = f.paper_json(&again);
    assert_eq!(
        paper["submitted"],
        json!(false),
        "状态文件坏了应视为「没有状态」，不能把学生挡在门外"
    );
    assert_eq!(paper["answers"], json!({}), "损坏的状态不应带出半截答案");
}

/// 用别的密钥加密的「状态文件」必须读不出来（诚实客户端假设：磁盘上的东西不可信）。
///
/// 为什么重要：状态文件可被同机其他账户替换（ACL 是第二道防线，不是唯一防线）。
/// 若实现「解不开也照用」，攻击者就能伪造 `submitted: true` 或塞入假答案。
#[test]
fn foreign_key_state_file_is_ignored() {
    let f = Fixture::new("foreign-key");
    let path = f.state_path();
    std::fs::create_dir_all(path.parent().expect("状态文件应在目录里")).expect("建目录失败");

    let other_key = [0x42u8; 32];
    let plain = json!({
        "student_id": STUDENT,
        "session_id": SESSION_ID,
        "answers": {"q-single": "B"},
        "submitted": true,
        "exported": "answers-forged.eea",
    })
    .to_string()
    .into_bytes();
    let sealed = crypto::encrypt(&other_key, &plain, b"web-state").expect("加密假状态失败");
    std::fs::write(&path, sealed).expect("写假状态文件失败");

    let token = f.login_ok(STUDENT);
    let paper = f.paper_json(&token);
    assert_eq!(paper["submitted"], json!(false), "换过密钥的状态必须被忽略");
    assert_eq!(paper["answers"], json!({}), "不能把伪造的答案读进来");
}

/// 落盘的状态文件必须是密文：不含 `answers` / `submitted` / 学号等明文特征。
///
/// 为什么重要：ACL 在 Unix 上只挡「其他用户」，在 Windows 上挡「其他账户」；
/// 一旦教师把 `web-answers/` 整个拷到 U 盘或上传网盘，只有加密还能保护作答内容。
#[test]
fn state_file_on_disk_is_ciphertext() {
    let f = Fixture::new("state-cipher");

    let token = f.login_ok(STUDENT);
    let qid = f.qids(&token)[0].clone();
    assert_status(&f.save(&token, &qid, "B"), 200, "保存应成功");
    assert_status(&f.submit(&token), 200, "交卷应成功");

    let raw = std::fs::read(f.state_path()).expect("状态文件应存在");
    assert!(!raw.is_empty(), "状态文件不应为空");
    let student_text = STUDENT.to_string();
    let needles: [&[u8]; 4] = [
        b"answers",
        b"submitted",
        b"exported",
        student_text.as_bytes(),
    ];
    for needle in needles {
        assert!(
            !raw.windows(needle.len()).any(|window| window == needle),
            "状态文件里出现了明文 {:?}（文件 {} 字节）：加密没生效？",
            String::from_utf8_lossy(needle),
            raw.len()
        );
    }
}

// ---------------------------------------------------------------------------
// 4. 端口占用
// ---------------------------------------------------------------------------

/// 端口被占时给出「换端口 / 关掉占用程序」的可操作提示。
///
/// 为什么重要：这是现场启动失败最常见的原因（上一场服务没关干净），
/// 只抛 `Address already in use` 老师看不懂，也没法自己解决。
#[test]
fn port_conflict_reports_actionable_error() {
    let held = bind_listener("127.0.0.1", 0).expect("先占住一个空闲端口");
    let port = held.local_addr().expect("取本地地址").port();

    let err = bind_listener("127.0.0.1", port).expect_err("同一端口第二次绑定必须失败");
    let text = format!("{err:#}");
    assert!(
        text.contains(&port.to_string()),
        "错误里要写出具体端口，现场才知道换哪个：{text}"
    );
    assert!(
        text.contains("占用"),
        "错误里要说明是「被占用」并给出下一步：{text}"
    );
}

/// 启动横幅里的地址：通配地址要换成局域网 IP，具体地址原样用。
///
/// 为什么重要：绑 127.0.0.1 调试时若还打印局域网 IP，老师把这个地址发给学生，
/// 学生连不上，而服务端日志看起来「一切正常」。
#[test]
fn banner_host_follows_bind_address() {
    assert_eq!(bind_host("127.0.0.1"), "127.0.0.1");
    assert_eq!(bind_host("192.168.1.5"), "192.168.1.5");
    let wildcard = bind_host("0.0.0.0");
    assert_eq!(wildcard, local_ip(), "0.0.0.0 要换成教师机的局域网地址");
    assert_ne!(wildcard, "0.0.0.0", "通配地址本身不能当访问地址");
}

/// 限速按「真实对端 IP」分桶，而不是一个全局桶。
///
/// 为什么重要：v1.4.5 及以前 `Request.peer` 被硬编码成 `127.0.0.1`——全场所有学生
/// 共用一个登录限速桶，40 人演练里第 21 个学生起全部 429，「按 IP 限速」等于没有。
/// 这条用例走真实 TCP：监听 `0.0.0.0`，用本机的**非回环地址**连进去，断言
/// `read_request` 报出来的是那个地址，而不是硬编码的回环地址。
#[test]
fn request_peer_comes_from_the_socket() {
    use std::io::Write as _;
    use std::net::{TcpListener, TcpStream};

    let target = local_ip();
    if target == "127.0.0.1" {
        // 没有非回环网卡（极少见）：这条用例没有判别力，直接返回而不是假绿。
        eprintln!("跳过：本机没有非回环地址，无法区分 127.0.0.1 与真实对端");
        return;
    }
    let listener = TcpListener::bind("0.0.0.0:0").expect("绑定监听端口");
    let port = listener.local_addr().expect("取监听地址").port();

    let mut client = TcpStream::connect((target.as_str(), port)).expect("连接自己的局域网地址");
    let (mut accepted, _) = listener.accept().expect("接受连接");
    client
        .write_all(b"GET / HTTP/1.1\r\nHost: drill\r\n\r\n")
        .expect("发送请求");
    let expected_peer = target.parse::<IpAddr>().expect("本机地址应能解析");
    let req = match read_request(&mut accepted, expected_peer).expect("解析请求") {
        ReadOutcome::Request(req) => req,
        other => panic!(
            "应解析出一个请求，实际 {}",
            match other {
                ReadOutcome::Closed => "Closed",
                ReadOutcome::Oversized(_) => "Oversized",
                ReadOutcome::Chunked => "Chunked",
                ReadOutcome::Request(_) => unreachable!(),
            }
        ),
    };

    let expected: IpAddr = target.parse().expect("本机地址应能解析");
    assert_eq!(req.peer, expected, "peer 必须是 TCP 对端地址");
    assert_ne!(
        req.peer,
        IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        "不得是硬编码的回环地址（那样全场共用一个限速桶）"
    );
}

// ---------------------------------------------------------------------------
// 5. 写接口：CSRF / 鉴权 / 请求体 / qid / 容量
// ---------------------------------------------------------------------------

/// 写答案接口的各类非法请求必须各自返回准确的状态码与文案。
///
/// 为什么重要：
/// - 缺 CSRF 校验 → 任何网页都能替学生改答案；
/// - 不校验 qid 是否属于本人卷 → 可以往答题卡里塞任意键，判分与核对全乱；
/// - 把客户端格式错误报成 401/500 → 现场只会看到「保存失败」，无从排查。
#[test]
fn invalid_write_requests_return_specific_errors() {
    let f = Fixture::new("bad-write");
    let token = f.login_ok(STUDENT);
    let qid = f.qids(&token)[0].clone();

    let no_csrf =
        f.route(&put_json("/api/answer", json!({"qid": qid, "value": "B"}), peer(1)).token(&token));
    assert_status(&no_csrf, 403, "缺 CSRF 头必须 403");
    assert!(
        err_msg(&no_csrf).contains("CSRF"),
        "实际：{}",
        err_msg(&no_csrf)
    );

    let no_cookie =
        f.route(&put_json("/api/answer", json!({"qid": qid, "value": "B"}), peer(1)).csrf());
    assert_status(&no_cookie, 401, "没有登录 cookie 应 401");

    let not_json = f.route(
        &request("PUT", "/api/answer", peer(1), b"not-json".to_vec())
            .csrf()
            .token(&token),
    );
    assert_status(&not_json, 400, "请求体不是 JSON 应 400");

    let no_qid = f.route(
        &put_json("/api/answer", json!({"value": "B"}), peer(1))
            .csrf()
            .token(&token),
    );
    assert_status(&no_qid, 400, "缺 qid 应 400");
    assert!(
        err_msg(&no_qid).contains("缺少 qid"),
        "实际：{}",
        err_msg(&no_qid)
    );

    let no_value = f.route(
        &put_json("/api/answer", json!({"qid": qid}), peer(1))
            .csrf()
            .token(&token),
    );
    assert_status(&no_value, 400, "缺 value 应 400");
    assert!(
        err_msg(&no_value).contains("缺少 value"),
        "实际：{}",
        err_msg(&no_value)
    );

    let unknown = f.save(&token, "q-does-not-exist", "B");
    assert_status(&unknown, 400, "不在本人卷里的 qid 必须拒绝");
    assert!(
        err_msg(&unknown).contains("不在本卷中"),
        "实际：{}",
        err_msg(&unknown)
    );
}

/// 单题超过 1 MiB 必须 413。
///
/// 为什么重要：代码题/问答可能很长，但单题 1 MiB 已经远超任何合理的作答；
/// 不设上限的话，一个 50 MB 的请求就能让服务端内存翻倍（每连接一线程）。
#[test]
fn single_answer_over_one_mib_is_rejected() {
    let f = Fixture::new("size-single");
    let token = f.login_ok(STUDENT);
    let qid = f.qids(&token)[0].clone();

    let too_big = f.save(&token, &qid, &"x".repeat(1024 * 1024 + 1));
    assert_status(&too_big, 413, "单题超过 1 MiB 必须 413");
    assert!(
        err_msg(&too_big).contains("1 MiB"),
        "实际：{}",
        err_msg(&too_big)
    );
}

/// 整卡累计超过 16 MiB 必须 413。
///
/// 为什么重要：单题上限挡不住「很多题各 1 MiB」——20 题就能堆到 20 MiB，
/// 而 `sweep_tick` 会克隆整份作答做锁外密封，内存会被放大一倍。
#[test]
fn whole_card_over_sixteen_mib_is_rejected() {
    // 单题上限 1 MiB，要突破整卡 16 MiB 只能靠题量：题库给 20 题、全抽。
    let f = Fixture::with_questions("size-card", 20, None);
    let token = f.login_ok(STUDENT);
    let qids = f.qids(&token);
    assert_eq!(qids.len(), 20, "抽题数应等于题库题量");

    // 前 16 题各 1 MB 直接铺进内存（见 `seed_answer` 的注释：走 HTTP 会写 136 MB 状态文件，
    // 在 Windows 上被杀毒扫描拖到 27 秒；这里要测的是 413 判定本身，不是写盘性能）。
    let chunk = "x".repeat(1_000_000); // < 1 MiB，单题合法
    for qid in &qids[..16] {
        f.seed_answer(&token, qid, &chunk);
    }
    let overflow = f.save(&token, &qids[16], &chunk);
    assert_status(&overflow, 413, "累计超过 16 MiB 必须 413");
    assert!(
        err_msg(&overflow).contains("16 MiB"),
        "实际：{}",
        err_msg(&overflow)
    );

    // 被拒绝的那份答案不能留在内存里：否则交卷时会被写进答题卡与状态文件、重启后还会被恢复，
    // 「拒绝」就成了假的（学生以为没存上，实际存上了）。
    let answers = f.paper_json(&token)["answers"].clone();
    assert!(
        answers.get(&qids[16]).is_none(),
        "413 拒绝后该题答案不得留在内存，实际：{answers}"
    );
}

/// 保存过的答案必须在**进程重启后**还在（教师机崩溃 / 断电 / 误杀进程）。
///
/// 为什么重要：v1.4.4 及以前，作答只在**交卷时**落盘，交卷前全在内存里——教师机崩一次，
/// 整场作答全丢，而文档一直写着「答案实时落盘」。这条用例模拟重启（同一批目录 + 全新
/// `App`，内存清零），重新登录后答案必须回来，且不能变成「已交卷」。
#[test]
fn answers_survive_a_restart_before_submit() {
    let mut f = Fixture::new("restart-recovery");
    let token = f.login_ok(STUDENT);
    let qids = f.qids(&token);
    assert_status(&f.save(&token, &qids[0], "B"), 200, "保存第一题应成功");
    assert_status(
        &f.save(&token, &qids[1], "第一行\n第二行"),
        200,
        "保存第二题应成功",
    );
    // 题目顺序由卷面决定，不假设题型：直接比对「重启前 / 重启后」两张卷的 answers。
    let before = f.paper_json(&token)["answers"].clone();
    assert_eq!(
        before.as_object().map_or(0, |m| m.len()),
        2,
        "保存后应有两题答案，实际：{before}"
    );
    assert!(
        f.state_path().is_file(),
        "保存后就该有加密状态文件：{}",
        f.state_path().display()
    );

    f.restart();

    let token2 = f.login_ok(STUDENT);
    let paper = f.paper_json(&token2);
    assert_eq!(paper["submitted"], false, "重启后不得变成已交卷");
    let after = paper["answers"].clone();
    assert_eq!(after, before, "重启后答案必须逐题恢复");
}

/// 事件日志必须能承受并发追加：每行都得是**完整 JSON**，不能丢行、不能拼行。
///
/// 为什么重要：交卷与保存由多个线程并发写 `events.jsonl`。v1.4.5 之前一次
/// `writeln!` 会拆成两次 `write`（正文 + 换行），并发时两行在字节层面交错——
/// 12 人同时交卷的演练里，12 条事件只有 2 条能解析，审计日志等于失效。
#[test]
fn concurrent_event_log_lines_stay_parseable() {
    let f = Fixture::new("event-log-race");
    let writers = 20;
    let per_writer = 5;
    std::thread::scope(|scope| {
        for writer in 0..writers {
            let app = Arc::clone(&f.app);
            scope.spawn(move || {
                for k in 0..per_writer {
                    app.log_event(2021001, "submit", &format!("answers-{writer}-{k}.eea"))
                        .expect("写事件日志失败");
                }
            });
        }
    });
    let events = f.events(); // 每行都 `serde_json::from_str`，解析失败会 panic
    assert_eq!(
        events.len(),
        writers * per_writer,
        "并发写事件不得丢行或拼行，实际 {} 行",
        events.len()
    );
    assert!(
        events.iter().all(|e| e["kind"] == "submit"),
        "每行都应带 kind=submit"
    );
}

/// 交卷后不能再改答案。
///
/// 为什么重要：答题卡在交卷时已经冻结落盘；若写接口还能改内存里的答案，
/// 就会出现「答题卡与状态文件不一致」，重启后又变成另一个版本。
#[test]
fn writing_after_submit_is_rejected() {
    let f = Fixture::new("after-submit");
    let token = f.login_ok(STUDENT);
    let qid = f.qids(&token)[0].clone();
    assert_status(&f.submit(&token), 200, "首次交卷应成功");

    let late = f.save(&token, &qid, "B");
    assert_status(&late, 409, "已交卷再改必须 409");
    assert!(
        err_msg(&late).contains("已交卷"),
        "实际：{}",
        err_msg(&late)
    );
}

/// 场次结束后不能再改答案（即使还没交卷）。
///
/// 为什么重要：到点自动收卷是后台线程做的，存在最多 15 秒的间隔；
/// 如果写接口不自己判一次窗口，学生可以在这段时间里继续改答案。
#[test]
fn writing_after_window_end_is_rejected() {
    let mut f = Fixture::new("after-window");
    let token = f.login_ok(STUDENT);
    let qid = f.qids(&token)[0].clone();
    f.expire_session();

    let late = f.save(&token, &qid, "B");
    assert_status(&late, 409, "窗口结束后写答案必须 409");
    assert!(
        err_msg(&late).contains("考试已结束"),
        "实际：{}",
        err_msg(&late)
    );
}

// ---------------------------------------------------------------------------
// 6. 交卷：自动收卷 / 幂等 / 断线恢复
// ---------------------------------------------------------------------------

/// 到点自动收卷必须写出答题卡、回填内存状态、记 1 条事件，且重复执行幂等。
///
/// 为什么重要：这是「学生忘了点交卷」的唯一兜底；如果它写出两份答题卡或重复记事件，
/// 收卷核对时会出现重复学生、判分表对不上。
#[test]
fn sweep_exports_expired_student_and_is_idempotent() {
    let mut f = Fixture::new("sweep");
    let token = f.login_ok(STUDENT);
    let qid = f.qids(&token)[0].clone();
    assert_status(&f.save(&token, &qid, "B"), 200, "保存应成功");
    f.expire_session();

    sweep_tick(&f.app);

    let files = f.eea_files();
    assert_eq!(
        files.len(),
        1,
        "到点应收走 1 份答题卡，实际 {:?}",
        file_names(&files)
    );
    let name = files[0]
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    assert!(
        name.starts_with(&format!("answers-{STUDENT}-{SESSION_ID}-")),
        "文件名要能定位到人/场：{name}"
    );

    {
        let state = f.app.state.lock().unwrap();
        let student = state
            .students
            .get(&token)
            .expect("自动收卷后学生仍在内存里");
        assert!(student.submitted, "自动收卷后必须标记已交卷");
        assert_eq!(
            student.exported.as_deref(),
            Some(name.as_str()),
            "内存里的导出文件名要与磁盘一致"
        );
    }

    let events = f.events();
    assert_eq!(events.len(), 1, "应只记 1 条 submit 事件，实际 {events:?}");
    assert_eq!(events[0]["kind"], json!("submit"));

    sweep_tick(&f.app);
    assert_eq!(f.eea_files().len(), 1, "重复 sweep 不得写出第二份答题卡");
    assert_eq!(f.events().len(), 1, "重复 sweep 不得重复记事件");
}

/// 重复交卷必须幂等：第二次 200 + `already`，磁盘上仍只有 1 份答题卡。
///
/// 为什么重要：学生会连点交卷按钮；如果第二次报错，学生会以为没交上而反复点，
/// 现场就会堵一整排人。若第二次重新生成答题卡，还可能出现两份时间戳不同的同名文件。
#[test]
fn duplicate_submit_is_idempotent() {
    let f = Fixture::new("dup-submit");
    let token = f.login_ok(STUDENT);

    let first = f.submit(&token);
    assert_status(&first, 200, "首次交卷应成功");
    let second = f.submit(&token);
    assert_status(&second, 200, "重复交卷应返回成功");
    assert_eq!(
        json_body(&second)["already"],
        json!(true),
        "第二次必须标记 already"
    );
    assert_eq!(
        json_body(&second)["file"],
        json_body(&first)["file"],
        "两次应指向同一份答题卡"
    );
    assert_eq!(f.eea_files().len(), 1, "只应写出 1 份答题卡");
}

/// 交卷后重新登录（断线恢复）必须还原答案与「已交卷」状态。
///
/// 为什么重要：浏览器崩溃、Wi-Fi 掉线、学生换设备都会触发重新登录；
/// 如果恢复不出来，学生会看到空白卷并重新作答，而答题卡早已冻结。
#[test]
fn relogin_after_submit_restores_answers() {
    let mut f = Fixture::new("relogin");
    let token = f.login_ok(STUDENT);
    let qids = f.qids(&token);
    assert_status(&f.save(&token, &qids[0], "B"), 200, "保存单选应成功");
    assert_status(
        &f.save(&token, &qids[1], "第一行答案\n第二行答案"),
        200,
        "保存填空应成功",
    );
    let before = f.paper_json(&token);
    let answers_before = before["answers"].clone();
    assert_ne!(answers_before, json!({}), "交卷前应能读到已保存的答案");
    assert_status(&f.submit(&token), 200, "交卷应成功");

    // 交卷后的「答案恢复」只在策略允许回看时才该发生（默认策略是仅教师可查看原卷，
    // 见 `submitted_paper_is_hidden_when_review_is_teacher_only`）。
    f.set_review(common::model::StudentReviewPolicy::Free);

    let again = f.login_ok(STUDENT);
    assert_ne!(again, token, "重新登录应换新 token（旧 token 被顶掉）");
    let after = f.paper_json(&again);
    assert_eq!(after["submitted"], json!(true), "重新登录后必须仍是已交卷");
    assert_eq!(after["answers"], answers_before, "答案必须与交卷前一致");
}

// ---------------------------------------------------------------------------
// 7. 读接口与首页
// ---------------------------------------------------------------------------

/// 未登录读接口 401；首页可访问且是 HTML。
///
/// 为什么重要：首页就是登录页，它 404/500 等于全场进不来；
/// 而 `/api/paper`、`/api/time` 若在未登录时返回 200，就等于把接口当公开读。
#[test]
fn unauthenticated_reads_and_index_page() {
    let f = Fixture::new("unauth");

    for path in ["/api/paper", "/api/time"] {
        let resp = f.route(&get(path, peer(1)));
        assert_status(&resp, 401, &format!("未登录访问 {path} 必须 401"));
    }

    let index = f.route(&get("/", peer(1)));
    assert_status(&index, 200, "首页应可访问（它就是登录页）");
    assert!(
        index.content_type.contains("text/html"),
        "首页必须是 HTML，实际 {}",
        index.content_type
    );
    assert!(
        !index.body.is_empty(),
        "首页内容不应为空（内嵌资源丢了会导致白屏）"
    );
}

// ---------------------------------------------------------------------------
// 8. 自动收卷不在锁内密封（v1.4.4 的锁分段回归）
// ---------------------------------------------------------------------------

/// 自动收卷的「加密 + 写盘」不得持有 `App::state` 锁。
///
/// 为什么重要：v1.4.3 及以前 `sweep_tick` 是一把锁从收集做到冻结，冻结一个人要做
/// AES-GCM + 两次写盘 + 一次 ACL 调用；这期间**所有学生**的每次请求都堵在锁上，
/// 50 人同场时表现为「页面点了没反应」，日志里什么都没有。
///
/// 这条用例不靠计时：`main.rs` 的 `seal_hook` 在进入密封阶段时回调一次，
/// 观察器在同线程里 `try_lock`——自己持着锁时必然 `Err`，所以旧实现必红、新实现必绿。
#[test]
fn automatic_export_does_not_hold_state_lock() {
    let mut f = Fixture::new("sweep-lock");
    let token = f.login_ok(STUDENT);
    let qid = f.qids(&token)[0].clone();
    assert_status(&f.save(&token, &qid, "B"), 200, "保存应成功");
    f.expire_session();

    let seen = Arc::new(AtomicBool::new(false));
    let lock_free = Arc::new(AtomicBool::new(false));
    {
        let app = Arc::clone(&f.app);
        let seen_at_seal = Arc::clone(&seen);
        let free_at_seal = Arc::clone(&lock_free);
        let _hook = HookGuard;
        seal_hook::set(Some(Arc::new(move |started: bool| {
            if started {
                seen_at_seal.store(true, Ordering::SeqCst);
                // 同线程持锁时 try_lock 必然失败，所以这里存的是「进入密封那一刻锁是否空闲」。
                free_at_seal.store(app.state.try_lock().is_ok(), Ordering::SeqCst);
            }
        })));
        sweep_tick(&f.app);
    }

    assert!(
        seen.load(Ordering::SeqCst),
        "sweep 必须真的走到密封阶段，否则这条断言是空转的"
    );
    assert!(
        lock_free.load(Ordering::SeqCst),
        "密封阶段不得持有 App::state 锁（旧实现「锁内冻结」在这里会是 false）"
    );
    assert_eq!(f.eea_files().len(), 1, "自动收卷仍应写出答题卡");
}

// ---------------------------------------------------------------------------
// 9. 窗口边界 / 收卷退避 / 写盘时序 / 请求体上限（v1.4.8）
// ---------------------------------------------------------------------------

/// 场次窗口是右开区间：`now == end` 必须算「已结束」。
///
/// 为什么单独测：改前服务端用 `now > end`、客户端用 `[start, end)`，同一秒两端
/// 判定相反（网页端能比离线端多答一秒）。边界只能靠纯函数断言，sleep 卡秒会飘。
#[test]
fn window_boundaries_are_half_open() {
    assert!(window_open(100, 100, 200), "start 那一秒算开着");
    assert!(window_open(199, 100, 200), "end 前 1 秒仍开着");
    assert!(!window_open(200, 100, 200), "end 那一秒必须算结束");
    assert!(!window_open(99, 100, 200), "start 之前不算");
    assert!(!window_open(201, 100, 200));
}

/// 集成层确认：窗口到点后登录被拒（`expire_session` 覆盖 end < now，这里覆盖 end ≤ now）。
#[test]
fn login_rejected_after_window_end() {
    let mut f = Fixture::new("window-end-login");
    let now = unix_now();
    f.set_window(now.saturating_sub(600), now);
    // 睡到下一整秒，确保 app.now() >= end（场次时间只有整秒精度）。
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert_status(&f.login(STUDENT), 401, "窗口结束后必须拒绝登录");
}

/// 自动收卷失败的退避：15→30→60→120→240→300 封顶。
#[test]
fn finalize_backoff_grows_then_caps() {
    assert_eq!(backoff_secs(1), 30);
    assert_eq!(backoff_secs(2), 60);
    assert_eq!(backoff_secs(3), 120);
    assert_eq!(backoff_secs(4), 240);
    assert_eq!(backoff_secs(5), 300);
    assert_eq!(backoff_secs(99), 300, "封顶 5 分钟");
}

/// 交卷之后，任何「未交卷」快照都不许再落盘。
///
/// 为什么：保存与交卷都在锁外写盘，保存的快照若后落地，会把「已交卷」覆盖回
/// 「未交卷」——重启后学生回到可编辑状态，已冻结的卡还会被同名校验覆盖。
#[test]
fn stale_snapshot_cannot_overwrite_submitted_state() {
    let f = Fixture::new("stale-snapshot");
    let token = f.login_ok(STUDENT);
    assert_status(&f.submit(&token), 200, "首次交卷应成功");

    let answers = BTreeMap::from([("q-single".to_string(), "B".to_string())]);
    let outcome = f
        .app
        .persist_state(
            Some(&token),
            STUDENT,
            SESSION_ID,
            &answers,
            false,
            None,
            0,
            true,
        )
        .expect("写盘不应报错");
    assert_eq!(
        outcome,
        PersistOutcome::SkippedSubmitted,
        "交卷后旧快照必须被丢弃"
    );

    let restored = f
        .app
        .load_state(STUDENT, SESSION_ID)
        .expect("状态文件应存在");
    assert!(restored.submitted, "磁盘状态必须还是已交卷");
    assert!(restored.exported.is_some(), "导出的文件名必须在");
}

/// 「已交卷」的幂等分支要校验卡文件完整性，不能只看文件是否存在。
#[test]
fn sheet_file_validity_requires_magic() {
    let f = Fixture::new("sheet-magic");
    let dir = f.app.answers_dir.clone();
    std::fs::create_dir_all(&dir).expect("建目录");

    let good = dir.join("good.eea");
    // 必须是**真正可打开**的卡：只造「魔数 + 0」正是旧实现漏掉的场景。
    let sheet = common::answer_sheet::AnswerSheet {
        version: common::answer_sheet::VERSION,
        student_id: STUDENT,
        session_id: SESSION_ID,
        paper_fingerprint: "0".repeat(common::answer_sheet::FP_LEN),
        qids: vec!["q-single".to_string()],
        answers: vec![Some("B".to_string())],
        updated_unix: unix_now(),
        submitted: false,
        submitted_unix: None,
        chosen_code_languages: Default::default(),
    };
    let sealed = common::answer_sheet::seal(&f.app.k_manifest, &sheet).expect("密封测试卡");
    std::fs::write(&good, &sealed).expect("写卡");
    assert!(is_valid_sheet(&f.app, &good), "真实卡应判定有效");

    let magic_only = dir.join("magic-only.eea");
    let mut bytes = Vec::from(&common::answer_sheet::MAGIC[..]);
    bytes.extend_from_slice(&[0u8; 32]);
    std::fs::write(&magic_only, &bytes).expect("写只有魔数的文件");
    assert!(
        !is_valid_sheet(&f.app, &magic_only),
        "只有魔数的截断卡必须判无效（否则学生永远交不了卷）"
    );

    let bad = dir.join("bad.eea");
    std::fs::write(&bad, b"NOTASHEET").expect("写坏卡");
    assert!(!is_valid_sheet(&f.app, &bad), "魔数不对必须判无效");

    let short = dir.join("short.eea");
    std::fs::write(&short, b"EES").expect("写半截卡");
    assert!(!is_valid_sheet(&f.app, &short), "半截卡必须判无效");

    assert!(
        !is_valid_sheet(&f.app, &dir.join("missing.eea")),
        "不存在必须判无效"
    );
}

/// 请求体超过上限：回 413，而不是把连接直接掐掉。
#[test]
fn oversized_body_gets_413() {
    use std::io::{Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};

    let f = Fixture::new("oversize-body");
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定");
    let port = listener.local_addr().expect("地址").port();
    let mut client = TcpStream::connect(("127.0.0.1", port)).expect("连接");
    let (server_side, _) = listener.accept().expect("accept");

    // 只发头，不发 body：服务端应在读到 Content-Length 时就决定 413。
    let head = format!(
        "POST /api/login HTTP/1.1\r\nHost: t\r\nContent-Length: {}\r\n\r\n",
        MAX_BODY_DEFAULT + 1
    );
    client.write_all(head.as_bytes()).expect("写请求头");
    handle(Arc::clone(&f.app), server_side).expect("处理请求");

    let mut buf = [0u8; 512];
    let n = client.read(&mut buf).expect("读响应");
    let text = String::from_utf8_lossy(&buf[..n]).to_string();
    assert!(
        text.starts_with("HTTP/1.1 413"),
        "超限必须回 413，实际：{}",
        &text[..text.len().min(80)]
    );
}

/// `Transfer-Encoding: chunked` 回 411（明确告诉客户端带 Content-Length）。
#[test]
fn chunked_body_gets_411() {
    use std::io::{Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};

    let f = Fixture::new("chunked-body");
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定");
    let port = listener.local_addr().expect("地址").port();
    let mut client = TcpStream::connect(("127.0.0.1", port)).expect("连接");
    let (server_side, _) = listener.accept().expect("accept");

    client
        .write_all(
            b"POST /api/login HTTP/1.1\r\nHost: t\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
        )
        .expect("写请求");
    handle(Arc::clone(&f.app), server_side).expect("处理请求");

    let mut buf = [0u8; 512];
    let n = client.read(&mut buf).expect("读响应");
    let text = String::from_utf8_lossy(&buf[..n]).to_string();
    assert!(
        text.starts_with("HTTP/1.1 411"),
        "chunked 必须回 411，实际：{}",
        &text[..text.len().min(80)]
    );
}

// ---------------------------------------------------------------------------
// 10. v1.4.9：严格时间接线 / 坏卡重写 / 保存与交卷的真实交错
// ---------------------------------------------------------------------------

/// 造一个 header 里的场次（只填与本组用例有关的字段）。
fn session_header(id: u32, start: &str, end: &str) -> container::SessionHeader {
    container::SessionHeader {
        session_id: id,
        start: start.into(),
        end: end.into(),
        data_off: 0,
        data_len: 0,
        envelopes: Vec::new(),
    }
}

/// 运行期也必须拒绝「没有时区偏移」的场次时间。
///
/// 为什么：`parse_rfc3339` 把无偏移时间当 UTC，UTC+8 的考场会整体偏 8 小时
/// （早开/晚收）且不报错。构建期有 `has_explicit_offset` 兜底，但按已接受风险
/// R-04 重加密过 header 的包会绕过它——运行期必须是同一口径。
#[test]
fn session_windows_reject_times_without_offset() {
    let bad = vec![session_header(
        7,
        "2026-12-20T09:00:00",
        "2026-12-20T11:00:00",
    )];
    let err = windows_from_sessions(&bad).expect_err("无时区偏移必须报错");
    assert!(err.contains("场次 #7"), "报错要带场次号：{err}");
    assert!(err.contains("+08:00"), "报错要给出写法示例：{err}");

    let reversed = vec![session_header(
        3,
        "2026-12-20T11:00:00+08:00",
        "2026-12-20T09:00:00+08:00",
    )];
    let err = windows_from_sessions(&reversed).expect_err("结束早于开始必须报错");
    assert!(err.contains("晚于"), "报错要说清原因：{err}");

    let ok = vec![session_header(
        1,
        "2026-12-20T09:00:00+08:00",
        "2026-12-20T11:00:00+08:00",
    )];
    let windows = windows_from_sessions(&ok).expect("带偏移应能解析");
    assert_eq!(windows.len(), 1);
}

/// 状态说「已交卷」但卡被截断时，下一次交卷必须**重写**而不是回 `already`。
///
/// 为什么：断电可能留下「魔数对、内容截断」的文件；只比魔数就会把它当有效，
/// 学生永远交不了卷、教师收卷时又打不开。
#[test]
fn broken_sheet_gets_re_exported_on_next_submit() {
    let f = Fixture::new("broken-sheet");
    let token = f.login_ok(STUDENT);
    let qid = f.qids(&token)[0].clone();
    assert_status(&f.save(&token, &qid, "B"), 200, "保存应成功");

    let first = f.submit(&token);
    assert_status(&first, 200, "首次交卷应成功");
    let name = body_json(&first)
        .get("file")
        .and_then(|f| f.as_str())
        .map(str::to_string)
        .expect("首次交卷应返回文件名");
    let path = f.app.answers_dir.join(&name);
    assert!(is_valid_sheet(&f.app, &path), "首次交卷应写出可打开的卡");

    // 模拟断电留下的半截文件：魔数在、内容没了。
    let mut bytes = Vec::from(&common::answer_sheet::MAGIC[..]);
    bytes.extend_from_slice(&[0u8; 32]);
    std::fs::write(&path, &bytes).expect("写坏卡");

    let second = f.submit(&token);
    assert_status(&second, 200, "坏卡时应重写并返回成功");
    let json = body_json(&second);
    assert!(
        json.get("already").is_none(),
        "坏卡不能走 already 分支：{json}"
    );
    assert!(is_valid_sheet(&f.app, &path), "重写后的卡必须能打开");
}

/// 并发保存与交卷：交错任意，磁盘最终必须是「已交卷」，且卡可打开。
///
/// 为什么：`persist_state` 的复查分支有单测，但真实交错（保存已过复查 → 交卷落盘
/// → 保存再写）没有任何用例固定过；这条用多线程把交错跑出来。
#[test]
fn concurrent_saves_and_a_submit_leave_a_submitted_state() {
    let f = Fixture::new("save-submit-race");
    let token = f.login_ok(STUDENT);
    let qids = f.qids(&token);
    let app = Arc::clone(&f.app);

    std::thread::scope(|scope| {
        for (index, qid) in qids.iter().enumerate().take(4) {
            let app = Arc::clone(&app);
            let token = token.clone();
            let qid = qid.clone();
            scope.spawn(move || {
                for round in 0..25 {
                    let req = put_json(
                        "/api/answer",
                        json!({"qid": qid, "value": format!("{index}-{round}")}),
                        peer(1),
                    )
                    .csrf()
                    .token(&token);
                    let resp = route(&app, &req);
                    // 交卷之后返回 409 是预期；其它状态码都是缺陷。
                    assert!(
                        resp.status == 200 || resp.status == 409,
                        "保存只能是 200 或 409，实际 {}",
                        resp.status
                    );
                }
            });
        }
        // 与保存同时发起交卷。
        let app_submit = Arc::clone(&app);
        let token_submit = token.clone();
        scope.spawn(move || {
            for _ in 0..3 {
                let req = post_json("/api/submit", json!({}), peer(1))
                    .csrf()
                    .token(&token_submit);
                let resp = route(&app_submit, &req);
                assert!(resp.status == 200, "交卷应成功，实际 {}", resp.status);
            }
        });
    });

    let restored = f
        .app
        .load_state(STUDENT, SESSION_ID)
        .expect("状态文件应存在");
    assert!(restored.submitted, "并发之后磁盘状态必须仍是已交卷");
    let name = restored.exported.expect("必须记录了导出的文件名");
    assert!(
        is_valid_sheet(&f.app, &f.app.answers_dir.join(&name)),
        "并发之后答题卡必须仍可打开"
    );
}

/// 413 必须在**客户端已经把 body 推进内核缓冲**时也能读到。
///
/// 为什么：回完 413 直接 close 会发 RST，客户端可能只看到「连接被重置」。
/// `drain_briefly` 把剩余数据读掉；这条用例真的把 body 发出去再读响应。
#[test]
fn oversized_request_still_gets_a_readable_413() {
    use std::io::{Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};

    let f = Fixture::new("oversize-with-body");
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定");
    let port = listener.local_addr().expect("地址").port();
    let mut client = TcpStream::connect(("127.0.0.1", port)).expect("连接");
    let (server_side, _) = listener.accept().expect("accept");

    // 故意多发 128 KiB body：既超上限，也确实进了对端接收缓冲。
    let body = vec![b'x'; 128 * 1024];
    let head = format!(
        "POST /api/login HTTP/1.1\r\nHost: t\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    client.write_all(head.as_bytes()).expect("写请求头");
    // 服务端会在读到长度后立刻回 413；body 由内核缓冲着。
    let mut client_writer = client.try_clone().expect("复制句柄");
    let sender = std::thread::spawn(move || {
        let _ = client_writer.write_all(&body);
    });

    handle(Arc::clone(&f.app), server_side).expect("处理请求");
    let _ = sender.join();

    let mut buf = [0u8; 512];
    let n = client.read(&mut buf).expect("读响应");
    let text = String::from_utf8_lossy(&buf[..n]).to_string();
    assert!(
        text.starts_with("HTTP/1.1 413"),
        "超限必须回可读的 413，实际：{}",
        &text[..text.len().min(80)]
    );
}

// ---------------------------------------------------------------------------
// 11. v1.4.11：网页端 ① 重登显示 JSON / ③ 回看策略
// ---------------------------------------------------------------------------

/// `paper` 必须同时给出「存储值」和「显示值」：填空/改错的存储是 JSON。
///
/// 为什么：网页端把 `answers` 直接塞进 textarea，学生重登/刷新后会看到
/// `["a","b"]`，一改就被二次编码，那题必错。
#[test]
fn paper_exposes_display_text_for_fill_and_fix() {
    let f = Fixture::new("answer-text");
    let token = f.login_ok(STUDENT);
    assert_status(
        &f.save(&token, "q-fill", "第一空\n第二空"),
        200,
        "保存应成功",
    );

    let data = f.paper_json(&token);
    let raw = data["answers"]["q-fill"]
        .as_str()
        .expect("存储值必须是字符串");
    assert!(raw.starts_with('['), "存储值应是 JSON 数组，实际 {raw}");
    assert_eq!(
        data["answer_text"]["q-fill"].as_str(),
        Some("第一空\n第二空"),
        "显示值必须是学生输入的样子，而不是 JSON"
    );
}

/// 默认策略（仅教师可查看原卷）：交卷后不再下发卷面。
#[test]
fn submitted_paper_is_hidden_when_review_is_teacher_only() {
    let f = Fixture::new("review-none");
    let token = f.login_ok(STUDENT);
    assert_status(&f.save(&token, "q-single", "B"), 200, "保存应成功");
    assert_status(&f.submit(&token), 200, "交卷应成功");

    let data = f.paper_json(&token);
    assert_eq!(data["submitted"], true);
    assert_eq!(data["review"], "none");
    assert!(data["paper"].is_null(), "默认策略下交卷后不得下发卷面");
    assert!(data["answers"].is_null(), "默认策略下交卷后不得下发作答");
}

/// 策略为「允许回看」且未到结束时间：交卷后仍可看到卷面（只读）。
#[test]
fn submitted_paper_is_visible_when_review_is_free() {
    let mut f = Fixture::new("review-free");
    let token = f.login_ok(STUDENT);
    assert_status(&f.save(&token, "q-single", "B"), 200, "保存应成功");
    assert_status(&f.submit(&token), 200, "交卷应成功");
    f.set_review(common::model::StudentReviewPolicy::Free);

    let data = f.paper_json(&token);
    assert_eq!(data["review"], "free");
    assert!(!data["paper"].is_null(), "允许回看时必须下发卷面");
    assert_eq!(data["answer_text"]["q-single"].as_str(), Some("B"));
}

/// 填空的存储/显示必须互逆，且按 `blank_count` 补齐。
///
/// 学生只填前两空时长度会短于期望值；补齐后判分口径与离线端一致（缺的算错），
/// 也不会因为 textarea 的尾随换行被 `lines()` 丢掉。
#[test]
fn fill_answer_text_round_trips_with_blank_count() {
    let item = picker::PaperItem {
        id: "q-fill".into(),
        qtype: "fill".into(),
        stem: String::new(),
        options: Vec::new(),
        body: common::model::QuestionBody::Template {
            template: "____①____ ____②____ ____③____".into(),
            blank_count: 3,
        },
        code_languages: Vec::new(),
        score: 2,
    };
    let stored = normalize_answer(&item, "a\nb");
    assert_eq!(stored, r#"["a","b",""]"#, "不足 blank_count 必须补齐");
    assert_eq!(answer_text(&item, &stored), "a\nb\n");
    // 再存一次必须稳定（幂等），否则学生每改一次就会多一个空
    assert_eq!(
        normalize_answer(&item, &answer_text(&item, &stored)),
        stored
    );
}

// ---------------------------------------------------------------------------
// 12. v1.4.12：会话 cookie（60 分钟滑动续期 / 提交即失效 / 退出即失效）
// ---------------------------------------------------------------------------

/// 取响应里的 `Set-Cookie`（没有就返回空串）。
fn set_cookie(resp: &Response) -> String {
    resp.headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
}

/// 登录必须下发「60 分钟、HttpOnly、SameSite=Strict」的 cookie。
///
/// 为什么是 60 分钟而不是会话 cookie：关掉浏览器就没了的话，学生重开要重新输学号；
/// 为什么不是「直到考试结束」：共享设备上会留下一个长期可续的会话。
#[test]
fn login_cookie_has_one_hour_ttl() {
    let f = Fixture::new("cookie-ttl");
    let resp = f.login(STUDENT);
    assert_status(&resp, 200, "登录应成功");
    let cookie = set_cookie(&resp);
    assert!(
        cookie.contains("Max-Age=3600"),
        "必须带 60 分钟 TTL：{cookie}"
    );
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Strict"), "{cookie}");
}

/// 前端每 15 秒轮询 `/api/time`，响应里必须顺带续期——否则 2 小时的考试会在
/// 第 60 分钟被 TTL 掐断（学生莫名其妙被要求重新登录）。
#[test]
fn time_poll_refreshes_the_session_cookie() {
    let f = Fixture::new("cookie-slide");
    let token = f.login_ok(STUDENT);
    let resp = f.route(&request("GET", "/api/time", peer(1), Vec::new()).token(&token));
    assert_status(&resp, 200, "取时应成功");
    let cookie = set_cookie(&resp);
    assert!(cookie.contains("Max-Age=3600"), "取时应续期：{cookie}");
    assert!(
        cookie.contains(&token),
        "续期的必须是同一个 token：{cookie}"
    );
}

/// 默认策略（仅教师可查看原卷）：交卷后会话立即失效。
#[test]
fn submit_expires_the_session_cookie() {
    let f = Fixture::new("cookie-submit");
    let token = f.login_ok(STUDENT);
    assert_status(&f.save(&token, "q-single", "B"), 200, "保存应成功");
    let resp = f.submit(&token);
    assert_status(&resp, 200, "交卷应成功");
    let cookie = set_cookie(&resp);
    assert!(cookie.contains("Max-Age=0"), "交卷后必须失效：{cookie}");
}

/// 允许回看时，交卷后会话保持（否则学生交完就看不到自己写的）。
#[test]
fn submit_keeps_the_cookie_when_review_is_free() {
    let mut f = Fixture::new("cookie-submit-free");
    let token = f.login_ok(STUDENT);
    assert_status(&f.save(&token, "q-single", "B"), 200, "保存应成功");
    f.set_review(common::model::StudentReviewPolicy::Free);
    let resp = f.submit(&token);
    assert_status(&resp, 200, "交卷应成功");
    let cookie = set_cookie(&resp);
    assert!(
        cookie.contains("Max-Age=3600"),
        "允许回看时必须保持会话：{cookie}"
    );
}

/// 退出：清 cookie，并把 token 从内存删掉（旧 cookie 立刻 401）。
#[test]
fn logout_clears_cookie_and_invalidates_token() {
    let f = Fixture::new("cookie-logout");
    let token = f.login_ok(STUDENT);
    assert_status(&f.save(&token, "q-single", "B"), 200, "保存应成功");

    let out = f.route(
        &post_json("/api/logout", json!({}), peer(1))
            .csrf()
            .token(&token),
    );
    assert_status(&out, 200, "退出应成功");
    assert!(
        set_cookie(&out).contains("Max-Age=0"),
        "退出必须清 cookie：{}",
        set_cookie(&out)
    );

    // 旧 token 立刻失效；答案在磁盘上，重登后仍在。
    let after = f.route(&request("GET", "/api/paper", peer(1), Vec::new()).token(&token));
    assert_status(&after, 401, "退出后旧 token 必须失效");
    let again = f.login_ok(STUDENT);
    let data = f.paper_json(&again);
    assert_eq!(
        data["answer_text"]["q-single"].as_str(),
        Some("B"),
        "退出不影响已保存的答案"
    );
}

/// 退出也要求 CSRF 头（否则任意网页都能把学生踢下线）。
#[test]
fn logout_requires_csrf() {
    let f = Fixture::new("cookie-logout-csrf");
    let token = f.login_ok(STUDENT);
    let resp = f.route(&post_json("/api/logout", json!({}), peer(1)).token(&token));
    assert_status(&resp, 403, "没有 CSRF 头必须拒绝");
    // 会话仍然可用
    assert_status(
        &f.route(&request("GET", "/api/paper", peer(1), Vec::new()).token(&token)),
        200,
        "被拒的退出不应影响会话",
    );
}

// ---------------------------------------------------------------------------
// 13. v1.5.0：TLS（自签证书 + 握手 + Secure cookie）
// ---------------------------------------------------------------------------

/// TLS 端到端：自签证书 → 客户端只信任这张证书 → 握手 → 登录请求走得通。
///
/// 这条用例同时验证三件容易错的事：
/// 1) 证书能被 rustls 客户端验证（SAN 含 IP、EKU 是 serverAuth）；
/// 2) `serve()` 在泛化到 `Read + Write` 后仍然正常处理 HTTP；
/// 3) TLS 模式下会话 cookie 带 `Secure`。
#[test]
fn tls_end_to_end_login_works() {
    use std::io::{Read as _, Write as _};
    use std::net::{IpAddr as StdIpAddr, Ipv4Addr, TcpListener};

    use rustls::pki_types::ServerName;

    // 注意：证书目录不能和 Fixture 的目录同名——Fixture::new 会先 remove_dir_all。
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/server-tests")
        .join(format!("tls-certs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let mut f = Fixture::new("tls-e2e");
    f.set_secure_cookies(true);
    let hosts = vec!["127.0.0.1".to_string()];
    let material = tls::load_or_generate(&dir, &hosts, 30).expect("生成自签证书");

    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定");
    let port = listener.local_addr().expect("地址").port();
    let app = Arc::clone(&f.app);
    let server_config = Arc::clone(&material.config);
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .expect("设超时");
        let conn = rustls::ServerConnection::new(server_config).expect("服务端 TLS 会话");
        let mut tls = rustls::StreamOwned::new(conn, stream);
        serve(&app, &mut tls, StdIpAddr::V4(Ipv4Addr::LOCALHOST), None).expect("处理请求");
    });

    // 客户端：根证书只放这一张自签证书（等价于学生点了「继续」之后的状态）
    let mut roots = rustls::RootCertStore::empty();
    let cert_pem = std::fs::read(&material.cert_path).expect("读证书");
    for cert in rustls_pemfile::certs(&mut &cert_pem[..]) {
        roots.add(cert.expect("证书可解析")).expect("加入根证书");
    }
    let client_config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server_name = ServerName::try_from("127.0.0.1").expect("服务器名");
    let conn = rustls::ClientConnection::new(Arc::new(client_config), server_name)
        .expect("客户端 TLS 会话");
    let tcp = std::net::TcpStream::connect(("127.0.0.1", port)).expect("连接");
    tcp.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .expect("设超时");
    let mut tls = rustls::StreamOwned::new(conn, tcp);

    let body = json!({"student_id": STUDENT}).to_string();
    let request = format!(
        "POST /api/login HTTP/1.1\r\nHost: 127.0.0.1\r\nX-EES-CSRF: 1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    tls.write_all(request.as_bytes()).expect("写请求");

    let mut response = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match tls.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                response.extend_from_slice(&chunk[..n]);
                // 收到完整响应体就够断言了（服务端是 Connection: close，读到 EOF 也行）
                if response.windows(2).any(|w| w == b"\r\n\r\n") && response.len() > 200 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    server.join().expect("服务端线程");

    let text = String::from_utf8_lossy(&response).to_string();
    assert!(
        text.starts_with("HTTP/1.1 200"),
        "TLS 上的登录应成功，实际响应：{}",
        &text[..text.len().min(120)]
    );
    assert!(text.contains("ees_token="), "应下发会话 cookie");
    assert!(
        text.contains("; Secure"),
        "TLS 模式下 cookie 必须带 Secure：{text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `Secure` 只在 TLS 模式下出现（明文模式下带 Secure 会让 cookie 永远发不出去）。
#[test]
fn secure_cookie_flag_follows_tls_mode() {
    let mut f = Fixture::new("cookie-secure");
    let token = f.login_ok(STUDENT);
    assert!(
        !set_cookie(&f.login(STUDENT)).contains("; Secure"),
        "明文模式不应带 Secure"
    );
    f.set_secure_cookies(true);
    let cookie = session_cookie(&f.app, &token);
    assert!(
        cookie.contains("; Secure"),
        "TLS 模式必须带 Secure：{cookie}"
    );
    let expired = expired_session_cookie(&f.app);
    assert!(
        expired.contains("; Secure"),
        "失效 cookie 也要带 Secure：{expired}"
    );
}

/// 明文 301 端口必须只回跳转，不能返回页面或数据。
///
/// 直接调生产函数 `handle_redirect_stream`——复制一份跳转逻辑来测等于没测。
#[test]
fn redirect_port_only_redirects() {
    use std::io::{Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};

    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定");
    let port = listener.local_addr().expect("地址").port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        handle_redirect_stream(&mut stream, "127.0.0.1", 8443);
    });

    let mut client = TcpStream::connect(("127.0.0.1", port)).expect("连接");
    client
        .write_all(b"GET /api/paper HTTP/1.1\r\nHost: t\r\n\r\n")
        .expect("写请求");
    let mut buf = [0u8; 512];
    let n = client.read(&mut buf).expect("读响应");
    server.join().expect("线程");
    let text = String::from_utf8_lossy(&buf[..n]).to_string();
    assert!(text.starts_with("HTTP/1.1 301"), "必须 301：{text}");
    assert!(
        text.contains("Location: https://127.0.0.1:8443/api/paper"),
        "跳转目标要保留路径：{text}"
    );
    assert!(!text.contains("<!doctype"), "明文端口不得返回页面：{text}");
}

// ---------------------------------------------------------------------------
// 14. v1.5.1：安全响应头 / TLS 走 handle() / 明文打 TLS 端口 / time 的 submitted
// ---------------------------------------------------------------------------

/// 生成一张测试用证书并返回配置（放 target/ 下，和 Fixture 的目录不同名）。
/// 测试用 TLS 材料：`Drop` 时清掉临时目录（以前每次跑测试都留一套证书）。
struct TestTls {
    config: Arc<rustls::ServerConfig>,
    cert_path: std::path::PathBuf,
    dir: std::path::PathBuf,
}

impl Drop for TestTls {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn test_tls(tag: &str) -> TestTls {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/server-tests")
        .join(format!("tls-cfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let material = tls::load_or_generate(&dir, &["127.0.0.1".to_string()], 30).expect("生成证书");
    TestTls {
        config: Arc::clone(&material.config),
        cert_path: material.cert_path,
        dir,
    }
}

/// 只信这张自签证书的客户端配置（真校验证书，不是跳过）。
///
/// 直接接收证书路径：以前按 `tag` 重新拼目录，改名/改目录就会静默读到不存在的文件。
fn trusting_client_config(cert_path: &std::path::Path) -> rustls::ClientConfig {
    let pem = std::fs::read(cert_path).expect("读测试证书");
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut &pem[..]) {
        roots.add(cert.expect("证书可解析")).expect("加入根证书");
    }
    rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth()
}

/// 所有响应（含 4xx）都必须带安全响应头。
///
/// CSP 能上到 `script-src 'self'` 的前提是页面不再有行内脚本——
/// 这也是为什么把 `app.js`/`app.css` 拆成独立资源。
#[test]
fn every_response_carries_security_headers() {
    for response in [
        Response::json(200, json!({"ok": true})),
        Response::error(413, "太大"),
        Response::error(411, "分块"),
        Response::error(401, "未登录"),
        Response::error(500, "内部错误"),
    ] {
        let mut buf: Vec<u8> = Vec::new();
        write_response(&mut buf, response).expect("写响应");
        let text = String::from_utf8_lossy(&buf).to_string();
        // 直接断言常量本身，而不是抄一份字面量：CSP 收紧时这里自动跟上
        // （v1.5.1 就因为测试里抄了旧字面量，CI 才抓到「收紧了但测试没跟上」）。
        for header in [
            format!("Content-Security-Policy: {CONTENT_SECURITY_POLICY}"),
            "X-Content-Type-Options: nosniff".to_string(),
            "X-Frame-Options: DENY".to_string(),
            "Referrer-Policy: no-referrer".to_string(),
            "Permissions-Policy: geolocation=(), camera=(), microphone=()".to_string(),
        ] {
            assert!(
                text.contains(&header),
                "缺少安全响应头 {header}；实际：\n{text}"
            );
        }
    }
}

/// 静态资源的缓存要分级：第三方 vendored 资源可缓存，我们自己的 app.js/app.css 不行。
///
/// 为什么必须分级：`write_response` 原来对**所有**响应写 `Cache-Control: no-store`，
/// 于是 174 KB 的 `codemirror.min.js` 每次刷新都要重下；而 `app.js`/`app.css` 随版本变，
/// 缓存一份旧的在学生机器上跑，会让「升级后行为不一致」这类问题极难排查。
#[test]
fn vendored_assets_are_cacheable_but_our_own_scripts_are_not() {
    let f = Fixture::new("cache-control");
    let headers_of = |path: &str| {
        let mut buf: Vec<u8> = Vec::new();
        write_response(&mut buf, f.route(&get(path, peer(1)))).expect("写响应");
        String::from_utf8_lossy(&buf).to_string()
    };

    let codemirror = headers_of("/assets/codemirror.js");
    assert!(
        codemirror.contains("Cache-Control: public, max-age=3600"),
        "第三方静态资源应允许缓存 1 小时：\n{codemirror}"
    );
    for path in ["/assets/app.js", "/assets/app.css", "/"] {
        let text = headers_of(path);
        assert!(
            text.contains("Cache-Control: no-store"),
            "{path} 必须 no-store（随版本变或承载会话）：\n{text}"
        );
    }
}

/// 真走 `handle()` 的 TLS 链路：握手 → 取首页 → 200 且带 CSP。
///
/// 之前只有 `serve()` 的用例，`ServerConnection::new` + `StreamOwned` 这段生产代码
/// 实际没被覆盖。
#[test]
fn tls_handle_serves_page_with_security_headers() {
    use std::io::{Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};

    use rustls::pki_types::ServerName;

    let mut f = Fixture::new("tls-handle");
    let tls_fixture = test_tls("handle");
    f.set_tls(Arc::clone(&tls_fixture.config));
    let app = Arc::clone(&f.app);

    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定");
    let port = listener.local_addr().expect("地址").port();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        handle(app, stream)
    });

    let client_config = trusting_client_config(&tls_fixture.cert_path);
    let server_name = ServerName::try_from("127.0.0.1").expect("服务器名");
    let conn = rustls::ClientConnection::new(Arc::new(client_config), server_name)
        .expect("客户端 TLS 会话");
    let tcp = TcpStream::connect(("127.0.0.1", port)).expect("连接");
    tcp.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .expect("设超时");
    let mut tls = rustls::StreamOwned::new(conn, tcp);

    tls.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .expect("写请求");
    let mut response = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match tls.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => response.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    let result = server.join().expect("服务端线程");
    assert!(result.is_ok(), "handle() 应成功：{result:?}");

    let text = String::from_utf8_lossy(&response).to_string();
    assert!(
        text.starts_with("HTTP/1.1 200"),
        "首页应 200：{}",
        &text[..text.len().min(120)]
    );
    assert!(text.contains("Content-Security-Policy:"), "首页必须带 CSP");
    assert!(text.contains("frame-ancestors 'none'"), "必须禁止被 iframe");
    assert!(
        text.contains("保密考试系统"),
        "返回的应是考试页面而不是错误页"
    );
}

/// 用明文（http://）访问 TLS 端口：`handle()` 只返回 Err，不 panic。
#[test]
fn plaintext_on_tls_port_is_rejected_without_panic() {
    use std::io::Write as _;
    use std::net::{TcpListener, TcpStream};

    let mut f = Fixture::new("tls-plain");
    let tls_fixture = test_tls("plain");
    f.set_tls(Arc::clone(&tls_fixture.config));
    let app = Arc::clone(&f.app);

    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定");
    let port = listener.local_addr().expect("地址").port();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        handle(app, stream)
    });

    let mut client = TcpStream::connect(("127.0.0.1", port)).expect("连接");
    client
        .write_all(b"GET / HTTP/1.1\r\nHost: t\r\n\r\n")
        .expect("写明文请求");
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("半关闭写端");
    let result = server.join().expect("服务端线程");
    assert!(result.is_err(), "明文打 TLS 端口必须返回 Err");
}

/// `/api/time` 必须报告 `submitted`：前端靠它发现「服务端已经自动收卷」。
#[test]
fn time_info_reports_submitted() {
    let f = Fixture::new("time-submitted");
    let token = f.login_ok(STUDENT);
    let before = f.route(&request("GET", "/api/time", peer(1), Vec::new()).token(&token));
    assert_status(&before, 200, "取时应成功");
    assert_eq!(body_json(&before)["submitted"], false);

    assert_status(&f.save(&token, "q-single", "B"), 200, "保存应成功");
    assert_status(&f.submit(&token), 200, "交卷应成功");

    let after = f.route(&request("GET", "/api/time", peer(1), Vec::new()).token(&token));
    assert_status(&after, 200, "交卷后取时仍应成功");
    assert_eq!(
        body_json(&after)["submitted"],
        true,
        "必须报告已交卷，前端才能切到已交卷页"
    );
}

/// 首页提示随模式变化：TLS 说自签，明文说「仅限排障」。
#[test]
fn index_hint_matches_tls_mode() {
    let mut f = Fixture::new("index-hint");
    assert!(
        index_html(&f.app).contains("明文 HTTP"),
        "明文模式必须有醒目提示"
    );
    let tls_fixture = test_tls("hint");
    f.set_tls(Arc::clone(&tls_fixture.config));
    let html = index_html(&f.app);
    assert!(
        html.contains("自签证书"),
        "TLS 模式应提示自签与「继续前往」"
    );
    assert!(!html.contains("明文 HTTP"), "TLS 模式不应再说明文");
}

// ---------------------------------------------------------------------------
// 14. v1.5.2：SAN 跟随 --bind、IPv6 URL、状态短语
// ---------------------------------------------------------------------------

/// `--bind` 指向具体地址时，该地址必须进 SAN。
///
/// 多网卡教师机「只绑机房侧网卡」是 v1.4.7 起的推荐用法；SAN 里没有它，
/// 学生就会看到「名称不匹配」——比自签本身更吓人。
#[test]
fn tls_hosts_include_the_bound_address() {
    let cli = Cli::parse_from(["exam-server", "--bind", "192.168.1.5"]);
    let hosts = tls_hosts(&cli);
    assert!(
        hosts.iter().any(|h| h == "192.168.1.5"),
        "绑定的地址必须进 SAN：{hosts:?}"
    );

    // 绑 0.0.0.0（默认）时不把通配地址写进 SAN——那不是学生访问的地址。
    let cli = Cli::parse_from(["exam-server"]);
    let hosts = tls_hosts(&cli);
    assert!(!hosts.iter().any(|h| h == "0.0.0.0"), "实际：{hosts:?}");
    assert!(hosts.iter().any(|h| h == "127.0.0.1"));
}

/// IPv6 字面量在 URL 里必须加方括号，否则不是合法 URL。
#[test]
fn url_host_brackets_ipv6_literals() {
    assert_eq!(url_host("::1"), "[::1]");
    assert_eq!(url_host("0:0:0:0:0:0:0:1"), "[0:0:0:0:0:0:0:1]");
    assert_eq!(url_host("192.168.1.5"), "192.168.1.5");
    assert_eq!(url_host("localhost"), "localhost");
    assert_eq!(url_host("[::1]"), "[::1]", "已经带方括号就不要重复加");
}

/// 403/405 必须写对理由短语（曾落到 `_ => "OK"`，日志里是 `403 OK`）。
#[test]
fn status_text_covers_403_and_405() {
    assert_eq!(status_text(403), "Forbidden");
    assert_eq!(status_text(405), "Method Not Allowed");
    assert_eq!(status_text(302), "Found");
}

/// 301 也要带 nosniff（其余安全头对无 body 的跳转没有意义）。
#[test]
fn redirect_301_carries_nosniff() {
    use std::io::{Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};

    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定");
    let port = listener.local_addr().expect("地址").port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        handle_redirect_stream(&mut stream, "::1", 8443);
    });

    let mut client = TcpStream::connect(("127.0.0.1", port)).expect("连接");
    client
        .write_all(b"GET /x?y=1 HTTP/1.1\r\nHost: t\r\n\r\n")
        .expect("写请求");
    let mut buf = [0u8; 512];
    let n = client.read(&mut buf).expect("读响应");
    server.join().expect("线程");
    let text = String::from_utf8_lossy(&buf[..n]).to_string();
    assert!(text.starts_with("HTTP/1.1 301"), "{text}");
    assert!(text.contains("X-Content-Type-Options: nosniff"), "{text}");
    assert!(
        text.contains("Location: https://[::1]:8443/x?y=1"),
        "IPv6 必须加方括号且保留路径：{text}"
    );
}

/// 网页端代码题的编辑器必须在 textarea 挂到 DOM **之后**初始化。
///
/// v1.4.11 把 `CodeMirror.fromTextArea` 挪进了 `renderText()`（那时 textarea 还没挂载），
/// CodeMirror 内部要 `parentNode.insertBefore`，直接抛 TypeError → 含代码题的整页渲染中断。
/// demo 题库没有代码题、冒烟与演练也不覆盖，这个 bug 活了四个版本（v1.4.11–v1.5.1）。
/// 这里用源码顺序断言守住它，避免同类别再犯（前端没有跑单测的运行时）。
#[test]
fn code_editor_is_initialised_only_after_the_textarea_is_in_the_dom() {
    let js = include_str!("../assets/app.js");
    let render_text = js
        .split("function renderText")
        .nth(1)
        .expect("app.js 里应有 renderText")
        .split("\nfunction ")
        .next()
        .expect("截取 renderText 函数体");
    assert!(
        !render_text.contains("CodeMirror.fromTextArea"),
        "renderText 里不能直接初始化 CodeMirror：那时 textarea 还不一定在 DOM 里"
    );
    let append = js
        .find("q.appendChild(ta)")
        .expect("render() 必须先把 textarea 挂到 DOM");
    let init = js
        .find("initCodeEditor(ta, item)")
        .expect("render() 必须在挂载后初始化编辑器");
    assert!(
        append < init,
        "必须先 appendChild(ta) 再 initCodeEditor(ta, item)"
    );
}

/// 重复的 `--tls-host` 要去重：否则同一个地址会报两条一模一样的自检警告。
#[test]
fn tls_hosts_are_deduplicated_after_normalisation() {
    let cli = Cli::parse_from([
        "exam-server",
        "--tls-host",
        "LOCALHOST",
        "--tls-host",
        "localhost",
    ]);
    let hosts = tls_hosts(&cli);
    let count = hosts
        .iter()
        .filter(|h| h.eq_ignore_ascii_case("localhost"))
        .count();
    assert_eq!(count, 1, "重复地址只该出现一次：{hosts:?}");
}

/// 后台 sweep 的抢占窗口（`submitted=true` 但 `exported` 还没有）里交卷必须回 503。
///
/// 以前这里返回 `200 {"already":true,"file":null}`：前端会显示「已交卷」，而那一刻
/// 答题卡还没落盘；若 sweep 随后失败还会回滚状态，监考看到的和学生对不上。
#[test]
fn submit_during_sweep_returns_503() {
    let fx = Fixture::new("sweep-503");
    let token = fx.login_ok(2021001);
    {
        let mut state = fx.app.lock_state();
        let st = state.students.get_mut(&token).expect("学生状态应存在");
        st.submitted = true;
        st.exported = None;
    }
    let resp = fx.route(
        &post_json("/api/submit", json!({}), peer(1))
            .token(&token)
            .csrf(),
    );
    assert_status(&resp, 503, "sweep 窗口内交卷应返回 503");
    assert!(
        body_json(&resp)["error"]
            .as_str()
            .unwrap_or_default()
            .contains("封存"),
        "文案要说明正在封存"
    );
}

// ---------------------------------------------------------------------------
// 监考页（`/monitor`）
// ---------------------------------------------------------------------------
//
// 这一组用例盯的是「监考最怕的三件事」：
// 1) 令牌能从学生侧被绕过（学生 cookie 不该能读监考数据）；
// 2) 收卷/终止把数据弄坏（必须复用 `seal_and_export`，产生的卡要能被 `answer_sheet::open` 读）；
// 3) 误点「终止」把学生永久锁在门外（必须能解除，且答案不丢）。

/// 无令牌 / 错令牌一律 401，不区分「没带」与「带错」。
#[test]
fn monitor_stats_requires_the_token() {
    let fx = Fixture::new("monitor-auth");
    let no_token = fx.route(&get("/api/monitor/stats", peer(1)));
    assert_status(&no_token, 401, "没带令牌应 401");

    let mut wrong = get("/api/monitor/stats", peer(1));
    wrong
        .headers
        .insert("x-monitor-token".into(), "b".repeat(64));
    assert_status(&fx.route(&wrong), 401, "令牌不对应 401");
}

/// 学生 cookie **不能**读监考数据：这是越权边界，必须有回归用例。
#[test]
fn monitor_stats_rejects_student_cookie() {
    let fx = Fixture::new("monitor-cookie");
    let token = fx.login_ok(STUDENT);
    let resp = fx.route(&get("/api/monitor/stats", peer(1)).token(&token));
    assert_status(&resp, 401, "学生 cookie 不能当监考令牌");
}

/// 名单里的人没登录也要列出来（监考最关心「谁还没进来」），并按学号升序。
#[test]
fn monitor_stats_lists_roster_and_states() {
    let mut fx = Fixture::new("monitor-roster");
    fx.set_roster(&[STUDENT, 2021002]);
    let token = fx.login_ok(STUDENT);
    let qid = fx.qids(&token)[0].clone();
    assert_status(&fx.save(&token, &qid, "答案"), 200, "保存应成功");

    let stats = fx.monitor_stats();
    assert_eq!(stats["counts"]["roster"], 2, "名单 2 人");
    assert_eq!(stats["counts"]["logged_in"], 1, "只登录了 1 人");
    assert_eq!(stats["counts"]["not_logged_in"], 1, "还有 1 人没登录");
    assert_eq!(stats["counts"]["online"], 1, "刚保存过，应算在线");

    let rows = stats["students"].as_array().expect("students 应是数组");
    assert_eq!(rows.len(), 2, "名单里的两人都要有行");
    assert_eq!(rows[0]["id"], STUDENT, "按学号升序，2021001 在前");
    assert_eq!(rows[0]["state"], "answering");
    assert_eq!(rows[0]["answered"], 1, "已答 1 题");
    assert!(rows[0]["total"].as_u64().unwrap_or(0) >= 3, "总题数应 >= 3");
    assert_eq!(rows[1]["id"], 2021002);
    assert_eq!(rows[1]["state"], "not_logged_in");
}

/// 「封存中」与「已交卷」必须是两个状态：前者意味着卡还没落盘，监考不能当成已完成。
#[test]
fn monitor_stats_distinguishes_sealing_from_submitted() {
    let fx = Fixture::new("monitor-sealing");
    let token = fx.login_ok(STUDENT);
    // 直接摆出 sweep 的中间态：submitted=true 但 exported=None。
    {
        let mut state = fx.app.lock_state();
        let st = state.students.get_mut(&token).expect("学生状态应存在");
        st.submitted = true;
        st.exported = None;
    }
    let stats = fx.monitor_stats();
    assert_eq!(stats["counts"]["sealing"], 1, "应识别出 1 个封存中");
    assert_eq!(stats["counts"]["submitted"], 1, "也算已交卷");
    assert_eq!(stats["counts"]["exported"], 0, "但还没有卡");
    assert_eq!(stats["students"][0]["state"], "sealing");

    // 撤回「封存中」这个人为状态，才轮到学生自己交卷（否则 submit 按设计返回 503）。
    {
        let mut state = fx.app.lock_state();
        let st = state.students.get_mut(&token).expect("学生状态应存在");
        st.submitted = false;
    }
    assert_status(&fx.submit(&token), 200, "真正交卷应成功");
    let stats = fx.monitor_stats();
    assert_eq!(stats["counts"]["sealing"], 0);
    assert_eq!(stats["counts"]["exported"], 1);
    assert_eq!(stats["students"][0]["state"], "submitted");
    assert!(
        stats["students"][0]["submitted_at"].as_u64().unwrap_or(0) > 0,
        "已封存应有封存时刻"
    );
}

/// 统计接口**只给计数**：绝不能把答案带出来（否则监考页变成泄题面）。
#[test]
fn monitor_stats_never_leaks_answers() {
    let fx = Fixture::new("monitor-noleak");
    let token = fx.login_ok(STUDENT);
    let qid = fx.qids(&token)[0].clone();
    let secret = "这是学生的私密作答-donotleak";
    assert_status(&fx.save(&token, &qid, secret), 200, "保存应成功");

    let resp = fx.monitor_get("/api/monitor/stats");
    let body = String::from_utf8_lossy(&resp.body).to_string();
    assert!(!body.contains(secret), "监考统计里出现了答案内容：{body}");
    assert!(!body.contains(&qid), "连题号都不该下发");
}

/// 监考页与它的两个静态资源都要能取到，且**不缓存**（脚本随版本变）。
#[test]
fn monitor_page_and_assets_are_no_store() {
    let fx = Fixture::new("monitor-page");
    let page = fx.route(&get("/monitor", peer(1)));
    assert_status(&page, 200, "监考页应可访问");
    assert_eq!(page.cache_control, "no-store", "页面不能缓存");
    assert!(!page.body.is_empty(), "页面不应为空");

    for path in ["/assets/monitor.js", "/assets/monitor.css"] {
        let resp = fx.route(&get(path, peer(1)));
        assert_status(&resp, 200, "静态资源应可访问");
        assert_eq!(resp.cache_control, "no-store", "{path} 不能缓存");
    }

    // 安全响应头走 `write_response` 统一出口：这里顺带确认 CSP 也在页面上。
    let mut buf: Vec<u8> = Vec::new();
    write_response(&mut buf, page).expect("写响应");
    let text = String::from_utf8_lossy(&buf).to_string();
    assert!(text.contains("Content-Security-Policy:"), "缺 CSP");
    assert!(text.contains("Cache-Control: no-store"), "缺 no-store");
}

/// 令牌文件：首次生成、之后复用、内容是 64 位 hex、（Unix）权限 0600。
#[test]
fn monitor_token_file_is_generated_and_reused() {
    let dir = test_dir("monitor-token");
    std::fs::create_dir_all(&dir).expect("建目录");
    let first = monitor::load_or_generate_token(&dir, None, false).expect("生成令牌");
    let second = monitor::load_or_generate_token(&dir, None, false).expect("复用令牌");
    assert_eq!(first, second, "重启后应复用同一个令牌（书签才有效）");
    assert_eq!(first.len(), 64, "令牌应是 32 字节 hex");
    assert!(first.bytes().all(|b| b.is_ascii_hexdigit()));
    let path = monitor::token_path(&dir);
    assert!(path.is_file(), "令牌文件应落盘");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path)
            .expect("读元数据")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "令牌文件应仅当前用户可读");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `--monitor-token` 覆盖优先；比较必须是精确匹配（前缀不算）。
#[test]
fn monitor_token_override_and_exact_match() {
    let dir = test_dir("monitor-override");
    std::fs::create_dir_all(&dir).expect("建目录");
    let custom = "f".repeat(64);
    assert_eq!(
        monitor::load_or_generate_token(&dir, Some(&custom), false).expect("命令行覆盖"),
        custom,
        "命令行覆盖应优先"
    );
    assert!(monitor::token_matches(&custom, &custom));
    assert!(
        !monitor::token_matches(&custom, &custom[..32]),
        "前缀不能通过"
    );
    assert!(!monitor::token_matches(&custom, &"e".repeat(64)));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 「登录时间」只记首次：重登（断线重连）不刷新，否则监考页的时间会一直往后跳。
#[test]
fn monitor_logged_in_at_keeps_first_login() {
    let fx = Fixture::new("monitor-login-time");
    let _first = fx.login_ok(STUDENT);
    let at1 = fx.monitor_stats()["students"][0]["logged_in_at"]
        .as_u64()
        .expect("应有登录时间");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let _second = fx.login_ok(STUDENT);
    let at2 = fx.monitor_stats()["students"][0]["logged_in_at"]
        .as_u64()
        .expect("应有登录时间");
    assert_eq!(at1, at2, "重登不应刷新首次登录时间");
}

/// 强制收卷：生成的卡必须能被 `answer_sheet::open` 读出（与学生交卷同一路径）。
#[test]
fn monitor_collect_seals_a_valid_sheet() {
    let fx = Fixture::new("monitor-collect");
    let token = fx.login_ok(STUDENT);
    let qids = fx.qids(&token);
    assert_status(
        &fx.save(&token, &qids[0], "监考收卷前的答案"),
        200,
        "保存应成功",
    );

    let resp = fx.monitor_post("/api/monitor/collect", json!({"student_id": STUDENT}));
    assert_status(&resp, 200, "收卷应成功");
    let name = body_json(&resp)["file"]
        .as_str()
        .unwrap_or_else(|| panic!("响应里应有 file"))
        .to_string();
    let path = fx.app.answers_dir.join(&name);
    let bytes = std::fs::read(&path).expect("答题卡应落盘");
    let sheet = answer_sheet::open(&fx.app.k_manifest, &bytes).expect("答题卡应可校验");
    assert_eq!(sheet.student_id, STUDENT);
    // 第 1 题是填空：服务端存的是 `encode_parts` 的 JSON 数组，不能直接与原文比较。
    let in_sheet = sheet.answers.iter().flatten().any(|raw| {
        answer_sheet::decode_parts(raw)
            .iter()
            .any(|part| part == "监考收卷前的答案")
    });
    assert!(in_sheet, "已保存的答案应进卡：{:?}", sheet.answers);

    // 学生侧立刻表现为「已交卷」。
    let paper = fx.route(&get("/api/paper", peer(1)).token(&token));
    assert_status(&paper, 200, "取卷仍应成功");
    assert_eq!(json_body(&paper)["submitted"], true);

    let events = fx.events();
    assert!(
        events
            .iter()
            .any(|e| e["ev"] == "monitor_collect" && e["student"] == STUDENT),
        "应有收卷审计行：{events:?}"
    );
}

/// 重复收卷是幂等的（返回 already，不再写盘）。
#[test]
fn monitor_collect_is_idempotent() {
    let fx = Fixture::new("monitor-collect-twice");
    let _token = fx.login_ok(STUDENT);
    let first = fx.monitor_post("/api/monitor/collect", json!({"student_id": STUDENT}));
    assert_status(&first, 200, "首次收卷应成功");
    let second = fx.monitor_post("/api/monitor/collect", json!({"student_id": STUDENT}));
    assert_status(&second, 200, "重复收卷也应成功");
    assert_eq!(json_body(&second)["already"], true, "第二次应标记 already");
    assert_eq!(fx.eea_files().len(), 1, "不应写出两份卡");
}

/// 批量收卷：服务端必须校验确认串（前端输入框挡不住 curl）。
#[test]
fn monitor_collect_all_requires_confirm_word() {
    let fx = Fixture::new("monitor-collect-all-confirm");
    let _token = fx.login_ok(STUDENT);

    let missing = fx.monitor_post("/api/monitor/collect-all", json!({}));
    assert_status(&missing, 400, "没有确认串应 400");
    let wrong = fx.monitor_post("/api/monitor/collect-all", json!({"confirm": "ok"}));
    assert_status(&wrong, 400, "确认串不对应 400");

    let ok = fx.monitor_post("/api/monitor/collect-all", json!({"confirm": "收卷"}));
    assert_status(&ok, 200, "确认串正确应成功");
    let body = body_json(&ok);
    assert_eq!(body["collected"].as_array().map(Vec::len), Some(1));
    assert_eq!(fx.eea_files().len(), 1, "应写出 1 份卡");
}

/// 批量收卷里「某人正被封存」只算那一人失败，不影响其他人。
#[test]
fn monitor_collect_all_reports_partial_failure() {
    let mut fx = Fixture::new("monitor-collect-all-partial");
    let token_a = fx.login_ok(STUDENT);
    // ① 把第一位摆成「封存中」（sweep 的中间态）：collect_one 会判失败，但不能影响别人。
    {
        let mut state = fx.app.lock_state();
        let st = state.students.get_mut(&token_a).expect("学生状态应存在");
        st.submitted = true;
        st.exported = None;
    }
    // ② 第二位直接插进状态表：再走一次登录会把第一位顶掉（同学号单会话），
    //    这里只需要一个「未封存」的会话存在即可。
    fx.insert_fake_student("fake-token-2021002", 2021002);
    let _ = &mut fx;
    let resp = fx.monitor_post("/api/monitor/collect-all", json!({"confirm": "收卷"}));
    assert_status(&resp, 200, "批量收卷整体应成功");
    let body = body_json(&resp);
    assert_eq!(
        body["failed"].as_array().map(Vec::len),
        Some(1),
        "封存中的那位应单独失败：{body}"
    );
    assert_eq!(
        body["collected"].as_array().map(Vec::len),
        Some(1),
        "另一位应被收卷：{body}"
    );
    assert_eq!(fx.eea_files().len(), 1, "只写出成功那位的卡");
}

/// 终止 = 先封存 → 再吊销 token → 再封禁；解除后能重新登录且答案还在。
#[test]
fn monitor_block_seals_revokes_and_can_be_undone() {
    let fx = Fixture::new("monitor-block");
    let token = fx.login_ok(STUDENT);
    let qids = fx.qids(&token);
    assert_status(
        &fx.save(&token, &qids[0], "终止前的答案"),
        200,
        "保存应成功",
    );

    let resp = fx.monitor_post(
        "/api/monitor/block",
        json!({"student_id": STUDENT, "reason": "违纪"}),
    );
    assert_status(&resp, 200, "终止应成功");
    assert!(
        body_json(&resp)["sealed"].as_str().is_some(),
        "终止前必须封存答题卡"
    );
    assert_eq!(fx.eea_files().len(), 1, "应有答题卡落盘");

    // ② token 已吊销
    let after = fx.route(&get("/api/paper", peer(1)).token(&token));
    assert_status(&after, 401, "被终止后旧 token 应立即失效");

    // ③ 重新登录被拒
    let relogin = fx.login(STUDENT);
    assert_status(&relogin, 403, "被终止的学生不能重新登录");
    assert!(
        err_msg(&relogin).contains("监考"),
        "文案要说清是监考终止的：{}",
        err_msg(&relogin)
    );

    // 解除后能登录，且答案仍在（封存的卡与状态文件都没被删）
    let unblock = fx.monitor_post("/api/monitor/unblock", json!({"student_id": STUDENT}));
    assert_status(&unblock, 200, "解除应成功");
    let token2 = fx.login_ok(STUDENT);
    // 终止时卡已封存：重登后按默认策略（仅教师可看原卷）不再下发卷面，
    // 所以「答案还在」要对着答题卡验证，而不是对着 /api/paper。
    let paper = fx.paper_json(&token2);
    assert_eq!(paper["submitted"], true, "解除后仍是已交卷：{paper}");
    let sheet_path = fx.eea_files();
    assert_eq!(sheet_path.len(), 1, "答题卡应仍在");
    let bytes = std::fs::read(&sheet_path[0]).expect("读答题卡");
    let sheet = answer_sheet::open(&fx.app.k_manifest, &bytes).expect("答题卡应仍可校验");
    let kept = sheet.answers.iter().flatten().any(|raw| {
        answer_sheet::decode_parts(raw)
            .iter()
            .any(|part| part == "终止前的答案")
    });
    assert!(kept, "解除后答题卡里的答案应还在：{:?}", sheet.answers);

    let events = fx.events();
    assert!(
        events.iter().any(|e| e["ev"] == "monitor_block"),
        "应有终止审计行"
    );
    assert!(
        events.iter().any(|e| e["ev"] == "monitor_unblock"),
        "应有解除审计行"
    );
}

/// 写操作要求 CSRF 头（与学生接口同口径）。
#[test]
fn monitor_write_requires_csrf_header() {
    let fx = Fixture::new("monitor-csrf");
    let _token = fx.login_ok(STUDENT);
    let mut req = post_json(
        "/api/monitor/collect",
        json!({"student_id": STUDENT}),
        peer(1),
    );
    req.headers
        .insert("x-monitor-token".into(), fx.app.monitor_token.clone());
    let resp = fx.route(&req);
    assert_status(&resp, 403, "缺 CSRF 头应 403");
    assert!(err_msg(&resp).contains("CSRF"), "文案应点名 CSRF");
}

/// 默认 `allow`：明文模式下也能收卷（产品决定：机房可能只跑明文排障）。
#[test]
fn monitor_write_allowed_in_plaintext_by_default() {
    let fx = Fixture::new("monitor-plaintext-allow");
    let _token = fx.login_ok(STUDENT);
    assert!(!fx.app.secure_cookies, "fixture 是明文模式");
    let resp = fx.monitor_post("/api/monitor/collect", json!({"student_id": STUDENT}));
    assert_status(&resp, 200, "默认 allow：明文下也应允许收卷");
}

/// `tls-only`：明文下写操作 403，但只读仍可用。
#[test]
fn monitor_tls_only_blocks_writes_in_plaintext() {
    let mut fx = Fixture::new("monitor-tls-only");
    let _token = fx.login_ok(STUDENT);
    fx.set_monitor_write_mode(monitor::MonitorWriteMode::TlsOnly);

    let stats = fx.monitor_get("/api/monitor/stats");
    assert_status(&stats, 200, "只读不受影响");
    assert_eq!(json_body(&stats)["write_mode"], "tls-only");
    assert_eq!(json_body(&stats)["plaintext_warning"], true);

    let resp = fx.monitor_post("/api/monitor/collect", json!({"student_id": STUDENT}));
    assert_status(&resp, 403, "明文 + tls-only 应拒绝写操作");
    assert!(
        err_msg(&resp).contains("write-mode"),
        "文案要告诉老师怎么开：{}",
        err_msg(&resp)
    );
}

/// 审计行只记令牌**指纹**，绝不能把令牌本身写进日志。
#[test]
fn monitor_audit_never_contains_the_token() {
    let fx = Fixture::new("monitor-audit");
    let _token = fx.login_ok(STUDENT);
    assert_status(
        &fx.monitor_post("/api/monitor/collect", json!({"student_id": STUDENT})),
        200,
        "收卷应成功",
    );
    let text =
        std::fs::read_to_string(fx.app.answers_dir.join("events.jsonl")).expect("事件日志应存在");
    assert!(
        !text.contains(&fx.app.monitor_token),
        "审计日志里出现了令牌明文：{text}"
    );
    let events = fx.events();
    let line = events
        .iter()
        .find(|e| e["ev"] == "monitor_collect")
        .expect("应有收卷审计行");
    let by = line["by"].as_str().expect("应有 by 字段");
    assert_eq!(by.len(), 8, "指纹应是 8 位 hex：{by}");
}

/// 未登录的学号去收卷 → 404（不是 500：这不是服务端错误）。
#[test]
fn monitor_collect_unknown_student_is_404() {
    let fx = Fixture::new("monitor-collect-unknown");
    let resp = fx.monitor_post("/api/monitor/collect", json!({"student_id": 999999}));
    assert_status(&resp, 404, "没有会话的学号应 404");
}

// ---------------------------------------------------------------- 监考页：安全面补充
// 这五条是 v1.8.0 复审发现的门禁缺口：越权只测了「读」、没测「写」；GET 调写；弱令牌；
// 令牌是否漏进响应体；收卷与学生交卷撞车时的语义。

/// 学生 cookie 不能调**写**接口（v1.8.0 只测了读接口，写接口漏了越权用例）。
#[test]
fn monitor_write_rejects_student_cookie() {
    let fx = Fixture::new("monitor-write-needs-token");
    let token = fx.login_ok(STUDENT);
    let mut req = post_json(
        "/api/monitor/collect",
        json!({"student_id": STUDENT}),
        peer(1),
    )
    .csrf();
    req.headers
        .insert("cookie".into(), format!("ees_token={token}"));
    assert_status(&fx.route(&req), 401, "学生 cookie 不能替学生收卷");
}

/// 写接口只认 POST：GET 落到 404（不存在的路由），不能被「顺手 GET 一下」触发。
#[test]
fn monitor_write_route_rejects_get() {
    let fx = Fixture::new("monitor-write-get-404");
    assert_status(
        &fx.monitor_get("/api/monitor/collect"),
        404,
        "GET 写接口应当不存在",
    );
    assert_status(
        &fx.monitor_get("/api/monitor/collect-all"),
        404,
        "GET 批量接口应当不存在",
    );
}

/// 弱令牌必须拒绝：`--monitor-token abc` 这种在局域网里 1 秒就能枚举出来。
#[test]
fn weak_monitor_token_is_rejected_unless_allowed() {
    let dir = test_dir("monitor-weak-token");
    let err =
        monitor::load_or_generate_token(&dir, Some("abc"), false).expect_err("弱令牌应被拒绝");
    assert!(
        format!("{err}").contains("monitor-token"),
        "错误文案要点名参数：{err}"
    );
    // 逃生阀：显式允许时照用（横幅会红字警告）。
    let allowed = monitor::load_or_generate_token(&dir, Some("abc"), true).expect("显式允许");
    assert_eq!(allowed, "abc");
    let _ = dir;
}

/// 令牌本身不能出现在任何响应体里（只在启动横幅与令牌文件里）。
#[test]
fn monitor_token_never_appears_in_responses() {
    let fx = Fixture::new("monitor-token-not-leaked");
    let _token = fx.login_ok(STUDENT);
    let token = fx.app.monitor_token.clone();
    for resp in [
        fx.monitor_get("/api/monitor/stats"),
        fx.monitor_post("/api/monitor/collect", json!({"student_id": STUDENT})),
        fx.monitor_get("/monitor"),
    ] {
        let body = String::from_utf8_lossy(&resp.body).to_string();
        assert!(!body.contains(&token), "响应体里出现了监考令牌明文");
    }
}

/// 收卷与学生交卷撞车：两种顺序都只产生**一份**答题卡，后来的一方拿到 `already`。
#[test]
fn monitor_collect_and_student_submit_do_not_double_write() {
    // 顺序 A：学生先交卷，监考再点收卷 → 监考拿到 already，不重复写盘。
    let fx = Fixture::new("monitor-vs-submit-student-first");
    let token = fx.login_ok(STUDENT);
    assert_status(&fx.submit(&token), 200, "学生交卷应成功");
    let resp = fx.monitor_post("/api/monitor/collect", json!({"student_id": STUDENT}));
    assert_status(&resp, 200, "已封存时监考收卷应为 200");
    let body = String::from_utf8_lossy(&resp.body).to_string();
    assert!(body.contains("already"), "应回报 already：{body}");
    assert_eq!(eea_count(&fx.app.answers_dir), 1, "只应有一份答题卡");

    // 顺序 B：监考先收卷，学生再点交卷 → 学生拿到 already，同样只有一份。
    drop(fx);
    let fx = Fixture::new("monitor-vs-submit-monitor-first");
    let token = fx.login_ok(STUDENT);
    assert_status(
        &fx.monitor_post("/api/monitor/collect", json!({"student_id": STUDENT})),
        200,
        "监考收卷应成功",
    );
    assert_status(&fx.submit(&token), 200, "之后学生交卷应返回已交卷（200）");
    assert_eq!(eea_count(&fx.app.answers_dir), 1, "只应有一份答题卡");
}

/// 答题卡目录里的 `.eea` 个数（撞车用例用它断言「只写一份」）。
fn eea_count(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .expect("读答题卡目录")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".eea"))
        .count()
}
