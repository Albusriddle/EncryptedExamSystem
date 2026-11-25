//! 监考页（`/monitor`）与它的五个接口：令牌、统计、收卷、批量收卷、终止/解除。
//!
//! 为什么单独一套令牌，而不是复用学生会话：学生 cookie 只能证明「这个浏览器是某个学生」，
//! 一旦让监考页认这个 cookie，任何一个学生只要打开 `/monitor` 就能看到全班的进度；反过来，
//! 监考页的令牌若下发成 cookie，它又会被浏览器的「同源自动附带」机制带进学生的写请求里。
//! 所以这里**只认请求头** `X-Monitor-Token`，不设任何 cookie。
//!
//! 令牌从哪里来：`<答题卡目录>/monitor.token`（首次启动生成 32 字节 hex 并 `harden_file`，
//! 之后重启复用——老师可以把监考页加书签）。`--monitor-token` 可覆盖，但它会出现在进程
//! 列表里，所以只作为应急手段。
//!
//! 写操作（收卷 / 批量收卷 / 终止 / 解除）为什么与只读分开判定：只读泄漏的后果是「进度被看到」，
//! 写操作泄漏的后果是「全班被替学生收卷」。所以写操作额外要求 CSRF 头，并受
//! `--monitor-write-mode` 约束（默认 `allow`，与产品决定一致：机房只跑明文排障时也能收卷；
//! 要求严格的部署可以切 `tls-only`）。

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::{
    App, FreezeSnapshot, Request, Response, Student, csrf_ok, is_valid_sheet, seal_and_export,
};

/// 监考令牌文件名（放在答题卡目录，与 `events.jsonl` 同级）。
const TOKEN_FILE: &str = "monitor.token";
/// 令牌应有的长度：32 字节 hex。
const TOKEN_HEX_LEN: usize = 64;
/// 在线判定：`last_seen` 在这么多秒以内算「在线」（学生端每 15 s 轮询 `/api/time`）。
const ONLINE_WINDOW_SECS: u64 = 60;
/// 请求头名（HTTP 头在 `Request.headers` 里已统一小写）。
const TOKEN_HEADER: &str = "x-monitor-token";
/// 批量收卷的确认串：服务端必须再校验一次，不能只靠前端。
const BULK_CONFIRM: &str = "收卷";

/// 监考页写操作的开关。
///
/// 默认 `Allow`：产品决定「机房只跑明文排障时也要能收卷」，代价是明文下令牌可被同网段
/// 嗅探——启动横幅会红字提示。要求严格的部署用 `--monitor-write-mode tls-only`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum MonitorWriteMode {
    /// 允许写操作（明文模式下也允许；横幅会警告）
    Allow,
    /// 只在 TLS 模式下允许写操作（明文下 403）
    TlsOnly,
}

impl MonitorWriteMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::TlsOnly => "tls-only",
        }
    }
}

// ---------------------------------------------------------------------------
// 令牌
// ---------------------------------------------------------------------------

pub(crate) fn token_path(answers_dir: &Path) -> PathBuf {
    answers_dir.join(TOKEN_FILE)
}

/// 读现有的监考令牌，没有就生成一个。
///
/// 为什么复用而不是每次随机：老师会把监考页加进书签、写进考场流程单；每次重启都变
/// 会让「考试中途教师机重启」变成一次「重新找令牌」的现场事故。
pub(crate) fn load_or_generate_token(answers_dir: &Path, override_token: Option<&str>) -> String {
    if let Some(token) = override_token {
        let token = token.trim().to_string();
        if !is_hex_token(&token) {
            eprintln!(
                "⚠ --monitor-token 不是 {TOKEN_HEX_LEN} 位 hex：仍按原样使用，但强度未知；\
                 且它会出现在进程列表里（`tasklist` 就能看到）"
            );
        }
        return token;
    }
    let path = token_path(answers_dir);
    if let Ok(text) = std::fs::read_to_string(&path) {
        let token = text.trim().to_string();
        if is_hex_token(&token) {
            return token;
        }
        // 文件坏了（手工改过、写了一半）：重新生成，不要让服务起不来。
        eprintln!(
            "⚠ 监考令牌文件内容不是 {TOKEN_HEX_LEN} 位 hex，已重新生成：{}",
            path.display()
        );
    }
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut bytes);
    let token = hex::encode(bytes);
    if let Err(e) = std::fs::write(&path, format!("{token}\n")) {
        eprintln!(
            "⚠ 监考令牌写盘失败 {}：{e}（重启后会换一个新令牌）",
            path.display()
        );
        return token;
    }
    // 与私钥同口径：能收紧就收紧（0600 / 受保护 DACL），失败只告警，不让考试起不来。
    if let Err(e) = crate::secure_fs::harden_file(&path) {
        eprintln!(
            "⚠ 监考令牌权限收紧失败（{e}）：{} 可能对其他本地用户可读",
            path.display()
        );
    }
    token
}

fn is_hex_token(text: &str) -> bool {
    text.len() == TOKEN_HEX_LEN && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 常量时间比较：逐字节 XOR 累积，不在第一个不同字节处提前返回。
///
/// 局域网上做时序攻击不现实，但这里比较的是「能替全班收卷」的凭据，多这 5 行不亏。
/// 长度不同直接返回（长度本身不是秘密）。
pub(crate) fn token_matches(expected: &str, given: &str) -> bool {
    let (a, b) = (expected.as_bytes(), given.as_bytes());
    if a.len() != b.len() || a.is_empty() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 令牌指纹：只把 SHA-256 的前 8 位 hex 写进审计日志。
///
/// 审计要能回答「是谁点的收卷」，但日志里不能出现可用的凭据——否则拿到 `events.jsonl`
/// 的人就能冒充监考。指纹足够区分「哪一台机器的令牌」。
pub(crate) fn token_fingerprint(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(&hasher.finalize()[..4])
}

// ---------------------------------------------------------------------------
// 鉴权
// ---------------------------------------------------------------------------

fn authorize(app: &App, req: &Request) -> Result<(), Response> {
    let Some(given) = req.headers.get(TOKEN_HEADER) else {
        // 不区分「没带」与「带错」：少给攻击者一个探路的信号。
        return Err(Response::error(401, "需要监考令牌"));
    };
    if token_matches(&app.monitor_token, given) {
        Ok(())
    } else {
        Err(Response::error(401, "需要监考令牌"))
    }
}

/// 写操作的准入：令牌 + CSRF 头 + 模式开关。
fn authorize_write(app: &App, req: &Request) -> Result<(), Response> {
    authorize(app, req)?;
    // 自定义头本身就是 CSRF 防线（跨站页面要带它会触发 CORS 预检，而本服务不发任何 CORS 头）；
    // 这里仍要求 `X-EES-CSRF`，与学生接口同一口径，也方便老师用 curl 时不会漏。
    if !csrf_ok(req) {
        return Err(Response::error(403, "缺少 CSRF 头"));
    }
    if !writes_allowed(app) {
        return Err(Response::error(
            403,
            "明文模式下已禁用写操作（--monitor-write-mode allow 可开启）",
        ));
    }
    Ok(())
}

fn writes_allowed(app: &App) -> bool {
    match app.monitor_write_mode {
        MonitorWriteMode::Allow => true,
        MonitorWriteMode::TlsOnly => app.tls.is_some(),
    }
}

// ---------------------------------------------------------------------------
// 读：统计快照
// ---------------------------------------------------------------------------

/// `GET /api/monitor/stats`
pub(crate) fn stats(app: &App, req: &Request) -> Response {
    if let Err(resp) = authorize(app, req) {
        return resp;
    }
    Response::json(200, snapshot(app))
}

/// 一次锁内取完整个快照。
///
/// 为什么必须一次锁：多次加锁会让「已登录人数」与「下面表格的行」来自不同时刻——现场
/// 会出现「上面写 28 人、下面数出 29 行」这种让监考怀疑数据的问题。
pub(crate) fn snapshot(app: &App) -> Value {
    let now = app.now();
    let state = app.lock_state();

    // 一个学号最多一个有效 token（重新登录会顶掉旧的），这里仍按学号归并一次，
    // 避免将来多条会话时表格出现重复行。
    let mut by_student: BTreeMap<u64, &Student> = BTreeMap::new();
    for student in state.students.values() {
        by_student.insert(student.student_id, student);
    }

    // 名单来源：`--roster` 给的学号 + 已经登录过的学号（后者保证「名单文件没给全」时
    // 也不会漏掉已经在答题的人）。
    let mut ids: Vec<u64> = app.roster.iter().copied().collect();
    for id in by_student.keys() {
        if !app.roster.contains(id) {
            ids.push(*id);
        }
    }
    ids.sort_unstable();
    ids.dedup();

    let mut online = 0usize;
    let mut submitted = 0usize;
    let mut exported = 0usize;
    let mut sealing = 0usize;
    let mut logged_in = 0usize;
    let mut rows: Vec<Value> = Vec::with_capacity(ids.len());

    for id in &ids {
        let blocked_reason = state.blocked.get(id).cloned();
        let Some(student) = by_student.get(id) else {
            // 从未登录过：名单里的人只能给出这个状态（见 `App::roster` 的注释）。
            rows.push(json!({
                "id": id,
                "session": 0,
                "state": "not_logged_in",
                "answered": 0,
                "total": 0,
                "last_seen": Value::Null,
                "logged_in_at": Value::Null,
                "submitted_at": Value::Null,
                "exported": Value::Null,
                "blocked_reason": blocked_reason,
            }));
            continue;
        };
        logged_in += 1;
        let is_sealing = student.submitted && student.exported.is_none();
        if student.submitted {
            submitted += 1;
        }
        if student.exported.is_some() {
            exported += 1;
        }
        if is_sealing {
            sealing += 1;
        }
        let age = now.saturating_sub(student.last_seen);
        if age <= ONLINE_WINDOW_SECS {
            online += 1;
        }
        let total = student
            .paper
            .sections
            .iter()
            .map(|section| section.items.len())
            .sum::<usize>();
        let answered = student
            .answers
            .values()
            .filter(|value| !value.trim().is_empty())
            .count();
        // 状态优先级：blocked > sealing > submitted > offline/answering。
        // blocked 优先是监考视角的需要：被终止的人即使卡已封存，也要标出来「是我干的」。
        let state_name = if blocked_reason.is_some() {
            "blocked"
        } else if is_sealing {
            "sealing"
        } else if student.submitted {
            "submitted"
        } else if age <= ONLINE_WINDOW_SECS {
            "answering"
        } else {
            "offline"
        };
        rows.push(json!({
            "id": id,
            "session": student.session_id,
            "state": state_name,
            "answered": answered,
            "total": total,
            // `last_seen` 是**距今秒数**（未登录为 null）——与前端约定一致；
            // 绝对值另给 `last_seen_unix`，免得前端自己算时踩到服务端时钟偏移。
            "last_seen": age,
            "last_seen_unix": student.last_seen,
            "logged_in_at": student.logged_in_at,
            "submitted_at": if student.submitted_at == 0 { Value::Null } else { json!(student.submitted_at) },
            "exported": student.exported,
            "blocked_reason": blocked_reason,
        }));
    }

    let (session_now, end_unix) = active_session(app, now);
    let roster = if app.roster_len > 0 {
        app.roster_len
    } else {
        ids.len()
    };
    let blocked = state.blocked.len();
    json!({
        "session_now": session_now,
        "server_time": now,
        "end_unix": end_unix,
        "write_mode": app.monitor_write_mode.as_str(),
        "plaintext_warning": app.tls.is_none(),
        "counts": {
            "roster": roster,
            "logged_in": logged_in,
            "online": online,
            "submitted": submitted,
            "exported": exported,
            "sealing": sealing,
            "not_logged_in": roster.saturating_sub(logged_in),
            "blocked": blocked,
        },
        "students": rows,
    })
}

/// 当前正在进行的场次与它的结束时间（没有活动场次时 `(0, 0)`）。
fn active_session(app: &App, now: u64) -> (u32, u64) {
    for (session_id, &(start, end)) in &app.windows {
        if crate::window_open(now, start, end) {
            return (*session_id, end);
        }
    }
    (0, 0)
}

// ---------------------------------------------------------------------------
// 写：收卷 / 批量收卷 / 终止 / 解除
// ---------------------------------------------------------------------------

enum CollectOutcome {
    /// 这次真的写了卡。
    Collected(String),
    /// 之前已经封存过（幂等）。
    Already(String),
    /// 该学号当前没有会话（没登录过，或已被踢下线）。
    NoSession,
    Failed(String),
}

/// 封存一个学生的答题卡（**不**碰「是否允许写」的判定，供单人与批量共用）。
///
/// 三段式与 `submit()` 完全一致：持锁抢占并克隆快照 → 锁外 `seal_and_export` → 回填。
/// 复用同一条导出路径是硬要求：否则监考收出来的 `.eea` 与学生自己交的可能格式/指纹不一致。
fn collect_one(app: &App, student_id: u64) -> CollectOutcome {
    let token = {
        let state = app.lock_state();
        state
            .students
            .iter()
            .find(|(_, student)| student.student_id == student_id)
            .map(|(token, _)| token.clone())
    };
    let Some(token) = token else {
        return CollectOutcome::NoSession;
    };
    let snapshot = {
        let mut state = app.lock_state();
        let Some(student) = state.students.get_mut(&token) else {
            return CollectOutcome::NoSession;
        };
        if student.submitted {
            if let Some(name) = student.exported.clone() {
                if is_valid_sheet(app, &app.answers_dir.join(&name)) {
                    return CollectOutcome::Already(name);
                }
                // 状态说已封存，但卡坏了/丢了：退回重写一份（与学生重复交卷同一口径）。
                student.exported = None;
                student.submitted = false;
            } else {
                // 自动收卷正持有「封存中」状态：让监考稍后再点，别与 sweep 抢同一份卡。
                return CollectOutcome::Failed("该生的答题卡正在封存中，请稍后再试".into());
            }
        }
        student.submitted = true;
        FreezeSnapshot {
            token: token.clone(),
            student_id,
            session_id: student.session_id,
            paper: student.paper.clone(),
            answers: student.answers.clone(),
        }
    };
    let now = app.now();
    let result = seal_and_export(
        app,
        snapshot.student_id,
        snapshot.session_id,
        &snapshot.paper,
        &snapshot.answers,
        now,
    );
    let mut state = app.lock_state();
    let Some(student) = state.students.get_mut(&snapshot.token) else {
        // 锁外被重新登录/踢下线顶掉：卡已经写出来了，照样如实回报。
        return match result {
            Ok(name) => CollectOutcome::Collected(name),
            Err(e) => CollectOutcome::Failed(e),
        };
    };
    match result {
        Ok(name) => {
            student.exported = Some(name.clone());
            student.submitted_at = now;
            student.last_seen = now;
            CollectOutcome::Collected(name)
        }
        Err(e) => {
            // 回滚抢占：否则这位学生会永远卡在「已交卷但没有卡」。
            student.submitted = false;
            CollectOutcome::Failed(e)
        }
    }
}

/// `POST /api/monitor/collect {"student_id": N}`
pub(crate) fn collect(app: &App, req: &Request) -> Response {
    if let Err(resp) = authorize_write(app, req) {
        return resp;
    }
    let Some(student_id) = body_student_id(req) else {
        return Response::error(400, "缺少 student_id");
    };
    match collect_one(app, student_id) {
        CollectOutcome::Collected(file) => {
            audit(app, "monitor_collect", student_id, Some(&file));
            Response::json(200, json!({"ok": true, "file": file}))
        }
        CollectOutcome::Already(file) => {
            Response::json(200, json!({"ok": true, "already": true, "file": file}))
        }
        CollectOutcome::NoSession => Response::error(404, "该学号当前没有会话（未登录或已被终止）"),
        CollectOutcome::Failed(e) => Response::error(500, e),
    }
}

/// `POST /api/monitor/collect-all {"confirm": "收卷"}`
///
/// 为什么要服务端再校验确认串：前端的输入框挡不住 `curl`。这个接口一次能封存全班，
/// 是整套监考页里唯一「一发不可收拾」的操作。
pub(crate) fn collect_all(app: &App, req: &Request) -> Response {
    if let Err(resp) = authorize_write(app, req) {
        return resp;
    }
    let body: Value = match serde_json::from_slice(&req.body) {
        Ok(value) => value,
        Err(_) => return Response::error(400, "请求不是合法 JSON"),
    };
    if body.get("confirm").and_then(Value::as_str) != Some(BULK_CONFIRM) {
        return Response::error(400, "缺少确认：请在确认框里输入「收卷」两个字");
    }
    // 目标：**所有**已登录的人，逐个交给 `collect_one` 判：
    // - 还没封存的 → 收卷；
    // - 已封存的 → 归入 `already`（幂等，不重复写盘）；
    // - 正被自动收卷持有的 → 归入 `failed`（"正在封存中，稍后再试"）。
    //
    // 为什么不在 SQL 层就过滤掉后两类：监考点「全部收卷」后需要看到**完整结果**
    // （「50 人里 47 人收好、2 人本来就好了、1 人正在封存」），静默省略会让他以为漏了谁。
    let targets: Vec<u64> = {
        let state = app.lock_state();
        let mut ids: Vec<u64> = state
            .students
            .values()
            .map(|student| student.student_id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    };
    let mut collected: Vec<u64> = Vec::new();
    let mut already: Vec<u64> = Vec::new();
    let mut failed: Vec<Value> = Vec::new();
    for id in targets {
        match collect_one(app, id) {
            CollectOutcome::Collected(file) => {
                audit(app, "monitor_collect", id, Some(&file));
                collected.push(id);
            }
            CollectOutcome::Already(_) => already.push(id),
            CollectOutcome::NoSession => {} // 期间被踢下线：不算失败
            CollectOutcome::Failed(e) => failed.push(json!({"id": id, "error": e})),
        }
    }
    audit(
        app,
        "monitor_collect_all",
        0,
        Some(&format!(
            "collected={} already={} failed={}",
            collected.len(),
            already.len(),
            failed.len()
        )),
    );
    Response::json(
        200,
        json!({"ok": true, "collected": collected, "already": already, "failed": failed}),
    )
}

/// `POST /api/monitor/block {"student_id": N, "reason": "…"}`
///
/// 顺序固定：**先封存、再吊销 token、最后封禁**。
/// - 先封存：否则「终止」会把学生已经写好的答案一起丢掉；
/// - 封存失败就整件事不生效（返回错误让监考重试），否则会出现「人被封了、卡没写」；
/// - 封禁（`blocked`）是必须的：只删 token 的话学生刷新一下又登回来，等于没终止。
pub(crate) fn block(app: &App, req: &Request) -> Response {
    if let Err(resp) = authorize_write(app, req) {
        return resp;
    }
    let body: Value = match serde_json::from_slice(&req.body) {
        Ok(value) => value,
        Err(_) => return Response::error(400, "请求不是合法 JSON"),
    };
    let Some(student_id) = body.get("student_id").and_then(Value::as_u64) else {
        return Response::error(400, "缺少 student_id");
    };
    let reason = body
        .get("reason")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("监考终止")
        .to_string();

    // ① 封存（没有会话时跳过：从未登录过的人没有卡可封）
    let sealed = match collect_one(app, student_id) {
        CollectOutcome::Collected(file) | CollectOutcome::Already(file) => Some(file),
        CollectOutcome::NoSession => None,
        CollectOutcome::Failed(e) => {
            return Response::error(500, format!("封存失败，未执行终止：{e}"));
        }
    };
    // ② 吊销 token（学生下一次请求即 401 → 页面提示重新登录）
    {
        let mut state = app.lock_state();
        if let Some(token) = state.tokens_by_student.remove(&student_id) {
            state.students.remove(&token);
        }
        // ③ 封禁：登录时直接 403
        state.blocked.insert(student_id, reason.clone());
    }
    audit(app, "monitor_block", student_id, sealed.as_deref());
    Response::json(200, json!({"ok": true, "sealed": sealed, "reason": reason}))
}

/// `POST /api/monitor/unblock {"student_id": N}`
///
/// 解除必须存在：误点一次「终止」就把学生锁在考场外，是最不可接受的现场事故。
/// 解除后答案仍在（状态文件与答题卡都不动），学生可以重新登录。
pub(crate) fn unblock(app: &App, req: &Request) -> Response {
    if let Err(resp) = authorize_write(app, req) {
        return resp;
    }
    let Some(student_id) = body_student_id(req) else {
        return Response::error(400, "缺少 student_id");
    };
    let removed = {
        let mut state = app.lock_state();
        state.blocked.remove(&student_id).is_some()
    };
    audit(app, "monitor_unblock", student_id, None);
    Response::json(200, json!({"ok": true, "removed": removed}))
}

/// 登录时的封禁检查（`main.rs::login` 调用）。
pub(crate) fn blocked_reason(app: &App, student_id: u64) -> Option<String> {
    let state = app.lock_state();
    state.blocked.get(&student_id).cloned()
}

fn body_student_id(req: &Request) -> Option<u64> {
    let body: Value = serde_json::from_slice(&req.body).ok()?;
    body.get("student_id").and_then(Value::as_u64)
}

/// 审计：写进 `events.jsonl`（与交卷同一个文件、同一把串行锁），**只记令牌指纹**。
fn audit(app: &App, event: &str, student_id: u64, detail: Option<&str>) {
    let line = json!({
        "ev": event,
        "student": student_id,
        "by": token_fingerprint(&app.monitor_token),
        "detail": detail,
        "at": app.now(),
    });
    if let Err(e) = app.append_event_line(&line) {
        // 审计失败不能把操作回滚（学生已经被封存/终止了），但必须留下痕迹。
        eprintln!("⚠ 监考操作审计写盘失败（{event} {student_id}）：{e}");
    }
}

// ---------------------------------------------------------------------------
// 页面
// ---------------------------------------------------------------------------

const MONITOR_HTML: &str = include_str!("monitor.html");

/// `GET /monitor`：页面本体。
///
/// 令牌不写进 HTML（`<!--TOKEN-->` 占位替换成空）：页面从 `?t=` 读一次、存进
/// `sessionStorage` 后立刻 `history.replaceState` 抹掉地址栏——否则它会留在浏览器历史里。
pub(crate) fn page(_app: &App) -> Response {
    Response::html_owned(MONITOR_HTML.replace("<!--TOKEN-->", ""))
}

/// 解析 `--roster` 文件（每行一个学号，`#` 开头为注释）。
///
/// 为什么需要这个文件：考试包的信封是**匿名**的（只有 tag + 密文，按学号派生入场密钥
/// 定位），运行时拿不到「名单里有哪些学号」。没有它，监考页只能显示「已登录的人」，
/// 显示不出「谁还没进来」——而那正是监考最需要的。名单文件由管理端/老师提供。
pub(crate) fn load_roster(path: &Path) -> anyhow::Result<HashSet<u64>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("读取名单文件 {} 失败：{e}", path.display()))?;
    let mut roster = HashSet::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let id: u64 = line.parse().map_err(|_| {
            anyhow::anyhow!(
                "名单文件 {} 第 {} 行不是学号：{line}",
                path.display(),
                index + 1
            )
        })?;
        roster.insert(id);
    }
    anyhow::ensure!(
        !roster.is_empty(),
        "名单文件 {} 里没有任何学号（每行一个学号，# 开头为注释）",
        path.display()
    );
    Ok(roster)
}
