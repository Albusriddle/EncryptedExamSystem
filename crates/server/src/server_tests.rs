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
            state: Mutex::new(State::default()),
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

    fn login_from(&self, peer: IpAddr, student: u64, pin: Option<&str>) -> Response {
        let body = match pin {
            Some(p) => json!({"student_id": student, "pin": p}),
            None => json!({"student_id": student}),
        };
        self.route(&post_json("/api/login", body, peer))
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

    fn state_path(&self) -> PathBuf {
        self.app
            .state_dir
            .join(format!("{STUDENT}-{SESSION_ID}.json"))
    }

    fn eea_files(&self) -> Vec<PathBuf> {
        files_with_ext(&self.app.answers_dir, "eea")
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

    /// 把场次窗口改成「已经结束」，模拟时间走过收卷时刻。
    ///
    /// 为什么不 `sleep`：场次时间是**整秒**，`end = 现在 + 1` 再 `sleep(1.2s)` 能否跨过
    /// 边界取决于构造 fixture 时的小数部分（实测会在 0–0.8 秒之间抖），这种用例迟早
    /// 变成 CI 里的偶发红灯。直接换一份 `end` 已过去的 payload 是确定性的，也快。
    ///
    /// 只替换 `App::payload`：已登录学生的内存试卷不受影响，场次窗口（`session_window`）
    /// 每次调用都从 payload 重新解析，所以下一次判定就会看到「已结束」。
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

    let no_id = f.route(&post_json("/api/login", json!({"pin": "1"}), peer(1)));
    assert_status(&no_id, 400, "缺 student_id 应是 400");
    assert!(
        err_msg(&no_id).contains("缺少 student_id"),
        "实际：{}",
        err_msg(&no_id)
    );

    let not_json = f.route(&request(
        "POST",
        "/api/login",
        peer(1),
        b"{not json".to_vec(),
    ));
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

    let chunk = "x".repeat(1_000_000); // < 1 MiB，单题合法
    for qid in &qids[..16] {
        assert_status(
            &f.save(&token, qid, &chunk),
            200,
            "前 16 题各 1 MB 应允许（累计 16 MB < 16 MiB）",
        );
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
    let f = Fixture::new("relogin");
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
