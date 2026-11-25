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
/// 封禁名单文件名（与令牌、事件日志同级）。
const BLOCKED_FILE: &str = "blocked.json";

/// 被封禁的学生：**必须落盘**。
///
/// 只放内存的话，教师机崩溃/重启后封禁就消失了——被终止的学生刷新一下又能登回来，
/// 而「重启」正是本项目一直在防的场景（v1.4.5 起作答状态就每次保存落盘了）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct BlockedEntry {
    pub reason: String,
    pub at: u64,
}

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
pub(crate) fn load_or_generate_token(
    answers_dir: &Path,
    override_token: Option<&str>,
    allow_weak: bool,
) -> anyhow::Result<String> {
    if let Some(token) = override_token {
        let token = token.trim().to_string();
        anyhow::ensure!(!token.is_empty(), "--monitor-token 不能为空");
        if !is_hex_token(&token) && !allow_weak {
            // 为什么拒绝而不是只告警：令牌等于「替全班收卷/终止」的权限，`abc` 这种
            // 一秒内就能猜中。告警只出现在启动日志里，老师很容易漏看，而且他本来就
            // 以为自己「已经设了令牌」。
            anyhow::bail!(
                "--monitor-token 必须是 {TOKEN_HEX_LEN} 位 hex（32 字节随机）。\n\
                 生成一个：openssl rand -hex 32\n\
                 或删掉 <answers-dir>/monitor.token 让服务自动生成；\n\
                 确实要用弱令牌（仅排障）请加 --monitor-allow-weak-token"
            );
        }
        return Ok(token);
    }
    let path = token_path(answers_dir);
    if let Ok(text) = std::fs::read_to_string(&path) {
        let token = text.trim().to_string();
        if is_hex_token(&token) {
            return Ok(token);
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
        return Ok(token);
    }
    // 与私钥同口径：能收紧就收紧（0600 / 受保护 DACL），失败只告警，不让考试起不来。
    if let Err(e) = crate::secure_fs::harden_file(&path) {
        eprintln!(
            "⚠ 监考令牌权限收紧失败（{e}）：{} 可能对其他本地用户可读",
            path.display()
        );
    }
    Ok(token)
}

pub(crate) fn is_hex_token(text: &str) -> bool {
    text.len() == TOKEN_HEX_LEN && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 常量时间比较：逐字节 XOR 累积，**长度不同也不提前返回**。
///
/// 局域网上做时序攻击不现实，但这里比较的是「能替全班收卷」的凭据，多这几行不亏。
/// 长度不等时按「短的一方补 0」跑完全程：否则「响应快 = 长度不对」能让扫描者
/// 区分出「服务端用的是 64 位令牌还是弱令牌」。
pub(crate) fn token_matches(expected: &str, given: &str) -> bool {
    let (a, b) = (expected.as_bytes(), given.as_bytes());
    if a.is_empty() {
        return false;
    }
    let mut diff: u8 = u8::from(a.len() != b.len());
    let rounds = a.len().max(b.len());
    for index in 0..rounds {
        let x = a.get(index).copied().unwrap_or(0);
        let y = b.get(index).copied().unwrap_or(0);
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

pub(crate) fn blocked_path(answers_dir: &Path) -> PathBuf {
    answers_dir.join(BLOCKED_FILE)
}

/// 读封禁名单；文件不存在/损坏都按空表处理（只告警）。
///
/// 为什么容错：监考页起不来比「封禁丢了」更糟；而损坏的文件已经在 stderr 里留了痕。
pub(crate) fn load_blocked(answers_dir: &Path) -> std::collections::HashMap<u64, BlockedEntry> {
    let path = blocked_path(answers_dir);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return std::collections::HashMap::new();
    };
    match serde_json::from_str::<std::collections::HashMap<String, BlockedEntry>>(&text) {
        Ok(raw) => raw
            .into_iter()
            .filter_map(|(id, entry)| Some((id.parse::<u64>().ok()?, entry)))
            .collect(),
        Err(e) => {
            eprintln!("⚠ 封禁名单 {} 解析失败（按空表继续）：{e}", path.display());
            std::collections::HashMap::new()
        }
    }
}

/// 把内存里的封禁表落盘（先写 tmp、`sync_all`、收紧权限、再 rename）。返回是否成功。
///
/// 失败只告警、不回滚那次终止：封禁在本次进程内仍然有效，操作也已经写进审计。
/// 为了「写不进盘」把老师刚做完的终止撤掉，比「重启后失效」更糟。
pub(crate) fn save_blocked(app: &App) -> bool {
    let snapshot: std::collections::BTreeMap<String, BlockedEntry> = {
        let state = app.lock_state();
        state
            .blocked
            .iter()
            .map(|(id, entry)| (id.to_string(), entry.clone()))
            .collect()
    };
    let path = blocked_path(&app.answers_dir);
    let tmp = path.with_extension("json.tmp");
    let bytes = match serde_json::to_vec_pretty(&snapshot) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("⚠ 封禁名单序列化失败：{e}");
            return false;
        }
    };
    let result = (|| -> std::io::Result<()> {
        {
            use std::io::Write as _;
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        // 与状态文件同口径：先收紧再改名，目标文件从出现的第一刻就是「仅当前用户」。
        let _ = crate::secure_fs::harden_file(&tmp);
        std::fs::rename(&tmp, &path)
    })();
    match result {
        Ok(()) => true,
        Err(e) => {
            eprintln!(
                "⚠ 封禁名单写盘失败 {}：{e}（本次进程内仍有效，重启后会丢失）",
                path.display()
            );
            false
        }
    }
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
    rate_limit_write(app)
}

/// 写操作冷却：同一令牌在冷却窗口内只允许一次写。
///
/// 为什么要：拿到令牌的脚本可以每秒发几十次 `collect-all`，每次都在磁盘上做 N 次
/// 「读 + 解密 + fsync」——把教师机磁盘打成瓶颈，学生自己的保存跟着变慢。
/// 读接口不限速（监考页每 15 s 轮询一次，本来就很轻）。
fn rate_limit_write(app: &App) -> Result<(), Response> {
    if app.monitor_write_cooldown_secs == 0 {
        return Ok(());
    }
    let now = app.now();
    let mut last = app
        .monitor_last_write
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(previous) = *last
        && now.saturating_sub(previous) < app.monitor_write_cooldown_secs
    {
        return Err(Response::error(429, "操作太频繁，请稍候 1 秒再试"));
    }
    *last = Some(now);
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
    let mut not_logged_in = 0usize;
    let mut rows: Vec<Value> = Vec::with_capacity(ids.len());

    for id in &ids {
        let blocked_reason = state.blocked.get(id).cloned();
        let Some(student) = by_student.get(id) else {
            // 从未登录过：名单里的人只能给出这个状态（见 `App::roster` 的注释）。
            not_logged_in += 1;
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
        // 有没有拿到名单：没拿到时 `counts.roster` 是「包里有多少个信封」，而表格只有
        // 已登录的人——两者对不上会让监考以为「一堆人没来」。页面据此显示「名单未提供」。
        "roster_known": app.roster_known,
        "counts": {
            "roster": roster,
            "logged_in": logged_in,
            "online": online,
            "submitted": submitted,
            "exported": exported,
            "sealing": sealing,
            // 按**行**统计：与 `students` 里 state=not_logged_in 的行数严格一致。
            // 原来用 `roster - logged_in` 在「没给名单」或「有学生不在名单里」时会对不上。
            "not_logged_in": not_logged_in,
            "blocked": blocked,
        },
        "students": rows,
    })
}

/// 当前正在进行的场次与它的结束时间（没有活动场次时 `(0, 0)`）。
fn active_session(app: &App, now: u64) -> (u32, u64) {
    // 多场次同时开放时（跨午休、补考与正常场重叠）必须**确定**地取一个：
    // `HashMap` 的迭代顺序每次都可能不同，否则页面上的倒计时会在两个场次之间乱跳。
    // 取「开始时间最早」的那个（先开始的先结束，监考最关心它）。
    let mut windows: Vec<(u32, u64, u64)> = app
        .windows
        .iter()
        .map(|(session_id, &(start, end))| (*session_id, start, end))
        .collect();
    windows.sort_unstable_by_key(|(session_id, start, _)| (*start, *session_id));
    for (session_id, start, end) in windows {
        if crate::window_open(now, start, end) {
            return (session_id, end);
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

    // 第一段（持锁）：只做内存操作——判重 + 抢占 + 克隆快照。
    // 同 `submit`：先取锁、拿完快照就把锁放掉，否则下面「卡坏了重写」的分支会自锁。
    let claim = {
        let mut state = app.lock_state();
        claim_for_collect(&mut state, &token)
    };
    let snapshot = match claim {
        CollectClaim::Sealed(snapshot) => snapshot,
        CollectClaim::Busy => {
            return CollectOutcome::Failed("该生的答题卡正在封存中，请稍后再试".into());
        }
        CollectClaim::Gone => return CollectOutcome::NoSession,
        CollectClaim::Already(name) => {
            // **锁外**校验：一次解密最大 16 MiB。原来在 `lock_state()` 里做，
            // 50 人批量收卷 = 50 次「读盘 + 解密」全程持全局锁，所有学生的保存/轮询
            // 都被堵住——v1.4.4 花力气去掉的「一个人堵全场」不能在这里回来。
            if is_valid_sheet(app, &app.answers_dir.join(&name)) {
                return CollectOutcome::Already(name);
            }
            // 状态说已封存，但卡坏了/丢了：退回重写一份（与学生重复交卷同一口径）。
            let retry = {
                let mut state = app.lock_state();
                reset_stale_export(&mut state, &token, &name);
                claim_for_collect(&mut state, &token)
            };
            match retry {
                CollectClaim::Sealed(snapshot) => snapshot,
                CollectClaim::Already(name) => return CollectOutcome::Already(name),
                CollectClaim::Busy => {
                    return CollectOutcome::Failed("该生的答题卡正在封存中，请稍后再试".into());
                }
                CollectClaim::Gone => return CollectOutcome::NoSession,
            }
        }
    };

    // 第二段（不持锁）：密封 + 写盘。
    let now = app.now();
    let result = seal_and_export(
        app,
        snapshot.student_id,
        snapshot.session_id,
        &snapshot.paper,
        &snapshot.answers,
        now,
    );

    // 第三段（持锁）：回填。
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
            // **不碰 `last_seen`**：它的语义是「学生最近一次请求」，用来算「在线」。
            // 收卷时刷新它，会把一个已经离线半小时的学生显示成「在线」——监考拿自己的
            // 操作污染自己的观测。
            CollectOutcome::Collected(name)
        }
        Err(e) => {
            // 回滚抢占：否则这位学生会永远卡在「已交卷但没有卡」。
            student.submitted = false;
            CollectOutcome::Failed(e)
        }
    }
}

/// 收卷的第一段（持锁）：判重 + 抢占。**不做任何磁盘/解密操作**。
enum CollectClaim {
    /// 抢到了「封存中」，快照可以拿到锁外去做密封。
    Sealed(FreezeSnapshot),
    /// 之前已经封存过，文件名待校验（校验在锁外做）。
    Already(String),
    /// 已被别的路径（学生交卷 / 自动收卷）置位但还没落盘。
    Busy,
    /// 这条会话已经不在了（被重新登录顶掉或被终止）。
    Gone,
}

fn claim_for_collect(state: &mut crate::State, token: &str) -> CollectClaim {
    let Some(student) = state.students.get_mut(token) else {
        return CollectClaim::Gone;
    };
    if student.submitted {
        return match student.exported.clone() {
            Some(name) => CollectClaim::Already(name),
            None => CollectClaim::Busy,
        };
    }
    student.submitted = true;
    CollectClaim::Sealed(FreezeSnapshot {
        token: token.to_string(),
        student_id: student.student_id,
        session_id: student.session_id,
        paper: student.paper.clone(),
        answers: student.answers.clone(),
    })
}

/// 把「状态说已封存、但卡坏了」的学生退回可封存状态。返回是否真的退回了。
fn reset_stale_export(state: &mut crate::State, token: &str, name: &str) -> bool {
    let Some(student) = state.students.get_mut(token) else {
        return false;
    };
    if student.submitted && student.exported.as_deref() == Some(name) {
        student.exported = None;
        student.submitted = false;
        true
    } else {
        false
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
            let audit = audit(app, "monitor_collect", student_id, Some(&file));
            Response::json(
                200,
                json!({"ok": true, "file": file, "audit": audit_field(audit)}),
            )
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
    let audit = audit(
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
        json!({
            "ok": true,
            "collected": collected,
            "already": already,
            "failed": failed,
            "audit": audit_field(audit),
        }),
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
    // ② 吊销 token + ③ 封禁：**同一段锁**内完成。
    //
    // 分开做的话，中间那一瞬学生的旧 token 还有效（能再保存一次），
    // 而登录检查又会在他「已经通过检查、还没写 token」时漏掉封禁（TOCTOU）。
    {
        let mut state = app.lock_state();
        if let Some(token) = state.tokens_by_student.remove(&student_id) {
            state.students.remove(&token);
        }
        // ③ 封禁：登录时直接 403
        state.blocked.insert(
            student_id,
            BlockedEntry {
                reason: reason.clone(),
                at: app.now(),
            },
        );
    }
    // 落盘：只放内存的话，教师机重启后封禁就没了——学生刷新一下又能登回来，
    // 而「重启」正是本项目一直在防的场景。
    let persisted = save_blocked(app);
    let audit = audit(app, "monitor_block", student_id, sealed.as_deref());
    Response::json(
        200,
        json!({
            "ok": true,
            "sealed": sealed,
            "reason": reason,
            "persisted": persisted,
            "audit": audit_field(audit),
        }),
    )
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
    // 解除也要落盘：否则重启后被解除的学生又会被拦住（与 block 对称）。
    let persisted = if removed { save_blocked(app) } else { true };
    let audit = audit(app, "monitor_unblock", student_id, None);
    Response::json(
        200,
        json!({
            "ok": true,
            "removed": removed,
            "persisted": persisted,
            "audit": audit_field(audit),
        }),
    )
}

/// 登录时的封禁检查（`main.rs::login` 调用）。
pub(crate) fn blocked_reason(app: &App, student_id: u64) -> Option<String> {
    let state = app.lock_state();
    state
        .blocked
        .get(&student_id)
        .map(|entry| entry.reason.clone())
}

/// 审计结果字段：前端/curl 一眼看出「这次操作有没有留下记录」。
fn audit_field(ok: bool) -> &'static str {
    if ok { "ok" } else { "failed" }
}

fn body_student_id(req: &Request) -> Option<u64> {
    let body: Value = serde_json::from_slice(&req.body).ok()?;
    body.get("student_id").and_then(Value::as_u64)
}

/// 审计：写进 `events.jsonl`（与交卷同一个文件、同一把串行锁），**只记令牌指纹**。
///
/// 返回是否写成功：失败时调用方要在响应里带 `"audit":"failed"`——操作本身不回滚
/// （学生已经被封存/终止了），但老师得知道「这次没有留下记录」。
fn audit(app: &App, event: &str, student_id: u64, detail: Option<&str>) -> bool {
    let line = json!({
        "ev": event,
        "student": student_id,
        "by": token_fingerprint(&app.monitor_token),
        "detail": detail,
        "at": app.now(),
    });
    match app.append_event_line(&line) {
        Ok(()) => true,
        Err(e) => {
            // 审计失败不能把操作回滚（学生已经被封存/终止了），但必须留下痕迹。
            eprintln!("⚠ 监考操作审计写盘失败（{event} {student_id}）：{e}");
            false
        }
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
