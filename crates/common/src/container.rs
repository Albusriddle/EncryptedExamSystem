//! 学生端单文件容器格式（v2）。
//!
//! 布局（自解压 exe 内嵌 payload，运行时读自身尾部）：
//!
//! ```text
//! [ stub 代码 ][ payload bytes ][ payload_len:u64 ][ "EXAMTAIL" ]
//!
//! payload = MAGIC(8) | version(u32) | header_len(u32) | header_ct | blob_area
//! ```
//!
//! ## v2 相对 v1 的安全改进
//!
//! 1. **包里不再存储学生名单**。v1 把 `students: [..]` 明文放进信封内层，
//!    任何持有 master 的人都能一次读出全班学号。v2 改为**每个学生一个信封**，
//!    信封里只有该生自己的学号和场次元信息，且用 `Argon2id(学号‖场次‖salt)`
//!    加密——**包里搜不到任何学号**。
//! 2. **每场独立会话密钥**。v1 所有场次共用一个 `master` 派生的密钥，
//!    泄漏一个场次等于全部沦陷。v2 每场随机生成 `S_i`，只存在于该场学生的
//!    信封里，题库用 `HKDF(S_i, "bank")` 加密——场次之间密码学隔离。
//! 3. **题库元信息（题量/pick）下沉到加密数据段**，解锁前不可见。
//!
//! ## 客户端解锁流程
//!
//! ```text
//! 取网络时间 → 找时间窗内的场次 → Argon2id(学号,场次,salt) → 算 tag
//!   → 按 tag 直接定位自己的信封（只算一次 Argon2）
//!   → 解出 S_i → 解 data_i → 抽题
//! ```

use std::collections::BTreeMap;

use rayon::prelude::*;

use zeroize::Zeroizing;

use crate::crypto::{self, CryptoError, KdfParams, TAG_LEN};
use crate::model::Session;

/// payload 起始魔数。
pub const MAGIC: &[u8; 8] = b"EXAMPKG3";
/// 尾部魔数，用于从 exe 末尾定位 payload。
pub const MAGIC_TAIL: &[u8; 8] = b"EXAMTAIL";
/// 当前格式版本。
pub const VERSION: u32 = 3;

/// 密钥区魔数：标记"这个包内嵌了 K_manifest"。
pub const KEY_MAGIC: &[u8; 8] = b"EXAMKEY1";

/// 密钥区长度：魔数 8 字节 + 混淆后的 32 字节密钥。
pub const KEY_AREA_LEN: usize = 8 + 32;

/// 密钥混淆掩码。
///
/// **这不是密码学保护**：它只让 `strings`/`grep` 搜不到 64 位 hex 形式的
/// K_manifest，让"随手看一眼"不能得手。有逆向能力的人仍可还原。
///
/// 安全性上与"编译期嵌入 binary"完全等价——两者都在同一个 exe 文件里，
/// 拿到文件的人都能取出密钥。真正的防线是：即使拿到 K_manifest，
/// 也只能读到场次时间与人数，**拿不到任何题目**（题库用每场独立
/// session_secret 加密，且需要知道合法学号并付出一次 Argon2id）。
const KEY_MASK: [u8; 32] = [
    0x3d, 0x9a, 0x57, 0xc1, 0x8e, 0x24, 0xfb, 0x60, 0xa5, 0x13, 0xd8, 0x4c, 0x72, 0xbe, 0x09, 0xe6,
    0x51, 0xcf, 0x2a, 0x87, 0x6d, 0xf0, 0x38, 0x94, 0x1b, 0xa2, 0x7e, 0xd5, 0x43, 0xbc, 0x68, 0x0f,
];

/// 把 K_manifest 打包成密钥区字节（魔数 + 掩码后的密钥）。
pub fn make_key_area(k_manifest: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(KEY_AREA_LEN);
    out.extend_from_slice(KEY_MAGIC);
    for i in 0..32 {
        out.push(k_manifest[i] ^ KEY_MASK[i]);
    }
    out
}

/// 从密钥区还原 K_manifest。密钥区格式不对时返回 `None`（视为未内嵌）。
pub fn read_key_area(area: &[u8]) -> Option<[u8; 32]> {
    if area.len() != KEY_AREA_LEN || &area[..8] != KEY_MAGIC {
        return None;
    }
    let mut k = [0u8; 32];
    for i in 0..32 {
        k[i] = area[8 + i] ^ KEY_MASK[i];
    }
    Some(k)
}

/// 容器错误。
#[derive(Debug, thiserror::Error)]
pub enum ContainerError {
    #[error("加密错误: {0}")]
    Crypto(#[from] CryptoError),
    #[error("格式错误: {0}")]
    Format(String),
    #[error("序列化错误: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}

/// 单个学生的信封：标签 + 密文。**不含学号明文**。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Envelope {
    /// `blake3(入场密钥)[..8]`，用于 O(1) 定位。
    #[serde(with = "tag_serde")]
    pub tag: [u8; TAG_LEN],
    /// base64( AEAD(入场密钥, student_id ‖ session_secret ‖ exams) )
    pub blob: String,
}

impl Envelope {
    /// 尝试用给定的入场密钥打开信封。
    fn open(&self, entry_key: &[u8; 32]) -> Result<Option<EntryPayload>, ContainerError> {
        if crypto::envelope_tag(entry_key) != self.tag {
            return Ok(None);
        }
        let blob = base64_decode(&self.blob)
            .map_err(|e| ContainerError::Format(format!("信封 base64 损坏: {e}")))?;
        let plain = crypto::decrypt(entry_key, &blob, b"envelope-v2")?;
        let payload: EntryPayload = serde_json::from_slice(&plain)?;
        Ok(Some(payload))
    }
}

/// 信封内层明文：该生自己的学号、场次密钥、场次考试配置。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct EntryPayload {
    student_id: u64,
    /// 本场会话密钥（hex）。用它派生题库解密密钥。
    session_secret: String,
    exams: Vec<crate::model::SessionExam>,
}

/// 场次的 header 部分。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionHeader {
    pub session_id: u32,
    pub start: String,
    pub end: String,
    /// 本场题库密文在 blob_area 内的偏移与长度。
    pub data_off: u32,
    pub data_len: u32,
    /// 每生一个信封，顺序无关、不含身份信息。
    pub envelopes: Vec<Envelope>,
}

/// header 明文结构（加密前）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Header {
    pub version: u32,
    pub salt: String,
    pub kdf: KdfParams,
    pub sessions: Vec<SessionHeader>,
}

/// 题库元信息（解锁后可见）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BankIndex {
    pub exam_id: String,
    pub title: String,
    pub total: usize,
    pub pick: u32,
}

/// data 段明文结构（解密后）：剥离答案的题库 + 元信息。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PayloadData {
    pub banks: Vec<crate::model::ExamBank>,
    /// exam_id -> 实际抽题数（清单设置的权威值）
    #[serde(default)]
    pub picks: BTreeMap<String, u32>,
    /// 题库元信息，解锁前不可见。
    #[serde(default)]
    pub bank_index: Vec<BankIndex>,
}

impl PayloadData {
    /// 按 exam_id 取实际抽题数，缺失时回退。
    pub fn pick_for(&self, exam_id: &str, fallback: u32) -> u32 {
        self.picks.get(exam_id).copied().unwrap_or(fallback)
    }
}

/// 解锁后的场次信息。
#[derive(Debug, Clone)]
pub struct UnlockedSession {
    pub session_id: u32,
    pub start: String,
    pub end: String,
    pub exams: Vec<crate::model::SessionExam>,
    /// 本场题库密文（已切好片）。
    pub data_ct: Vec<u8>,
    /// 本场会话密钥。
    pub session_secret: Zeroizing<[u8; 32]>,
}

/// 打包：把题库与场次封装成 payload 字节串。
/// 构建 payload。
///
/// `embed_k_manifest = true` 时把 K_manifest 写进密钥区，
/// 学生端**双击即可解锁**，无需任何参数，也无需教师重新编译 client。
/// 这是默认路径：否则教师必须装 Rust 工具链才能产出可用的包。
pub fn build_payload(
    master: &[u8; 32],
    banks: &[crate::model::ExamBank],
    sessions: &[Session],
    salt: &[u8],
    kdf: KdfParams,
    embed_k_manifest: bool,
) -> Result<Vec<u8>, ContainerError> {
    // 1. 剥离答案。
    let mut clean_banks = banks.to_vec();
    for bank in &mut clean_banks {
        for q in &mut bank.questions {
            q.answer = None;
        }
    }

    // 2. 清单自身校验（时间窗、重复学号、场次编号）。
    //    用静默版：builder 层已校验并打印过警告，这里重复打印会让管理员困惑。
    validate_sessions_silent(sessions)?;

    // 3. 逐场生成：会话密钥 → 题库密文 → 每生信封。
    let mut blob_area: Vec<u8> = Vec::new();
    let mut session_headers = Vec::with_capacity(sessions.len());

    for s in sessions {
        let session_secret = crypto::random_key();
        let bank_key = crypto::hkdf_derive(&session_secret, salt, "bank");

        // 本场的抽题数：**必须按场次独立解析**。
        // 早期实现用全局 exam_id→pick 映射并取 max，导致场次 1 设 2 题
        // 会被场次 2 的 5 题污染（实测：清单要求 2 题，实际抽了 5 题）。
        let picks = resolve_session_picks(&clean_banks, s)?;

        // 本场引用的题库（按清单顺序）
        let mut session_banks = Vec::new();
        let mut session_index = Vec::new();
        for e in &s.exams {
            let bank = match_bank(&e.file, &clean_banks).ok_or_else(|| {
                ContainerError::Format(format!(
                    "场次 {} 引用的题库 {} 匹配不到（请检查清单的 file 与题库 exam_id）",
                    s.session_id, e.file
                ))
            })?;
            let pick = picks.get(&bank.exam_id).copied().unwrap_or(bank.pick);
            if pick as usize > bank.questions.len() {
                return Err(ContainerError::Format(format!(
                    "场次 {} 的题库 {} 要求抽 {pick} 题，但只有 {} 题",
                    s.session_id,
                    bank.exam_id,
                    bank.questions.len()
                )));
            }
            session_banks.push(bank.clone());
            session_index.push(BankIndex {
                exam_id: bank.exam_id.clone(),
                title: bank.title.clone(),
                total: bank.questions.len(),
                pick,
            });
        }

        let data = PayloadData {
            banks: session_banks,
            picks,
            bank_index: session_index,
        };
        let data_plain = serde_json::to_vec(&data)?;
        let data_ct = crypto::encrypt(&bank_key, &data_plain, b"session-data")?;

        let data_off = blob_area.len() as u32;
        let data_len = data_ct.len() as u32;
        blob_area.extend_from_slice(&data_ct);

        // 每生一个信封。
        //
        // **并行生成**：每生一次 Argon2id(64MiB, t=3) 约 91ms，
        // 1000 人串行要 91 秒（实测）。每个信封完全独立：
        // 输入只有 (sid, session_id, salt, kdf, session_secret)，
        // session_secret 在循环外生成且只读，Argon2id 无全局状态，
        // 因此并行化没有数据竞争风险。
        let envelopes: Vec<Envelope> = s
            .students
            .par_iter()
            .map(|&sid| -> Result<Envelope, ContainerError> {
                let entry_key = crypto::session_entry_key(sid, s.session_id, salt, kdf)?;
                let inner = EntryPayload {
                    student_id: sid,
                    session_secret: hex::encode(*session_secret),
                    exams: s.exams.clone(),
                };
                let inner_plain = serde_json::to_vec(&inner)?;
                let blob = crypto::encrypt(&entry_key, &inner_plain, b"envelope-v2")?;
                Ok(Envelope {
                    tag: crypto::envelope_tag(&entry_key),
                    blob: base64_encode(&blob),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        session_headers.push(SessionHeader {
            session_id: s.session_id,
            start: s.start.clone(),
            end: s.end.clone(),
            data_off,
            data_len,
            envelopes,
        });
    }

    // 4. header 加密。K_manifest 用空 salt 派生（header 里才带真实 salt）。
    let k_manifest = manifest_key(master);
    let header = Header {
        version: VERSION,
        salt: hex::encode(salt),
        kdf,
        sessions: session_headers,
    };
    let header_plain = serde_json::to_vec(&header)?;
    let header_ct = crypto::encrypt(&k_manifest, &header_plain, b"header-v3")?;

    // 5. 组装 payload。
    //    布局：MAGIC | version | header_len | header_ct | key_len | key_area | blob_area
    //    key_len = 0 表示未内嵌密钥（学生需自行提供 master）。
    let key_area: Vec<u8> = if embed_k_manifest {
        make_key_area(&k_manifest)
    } else {
        Vec::new()
    };
    let mut out = Vec::with_capacity(20 + header_ct.len() + key_area.len() + blob_area.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(header_ct.len() as u32).to_le_bytes());
    out.extend_from_slice(&header_ct);
    out.extend_from_slice(&(key_area.len() as u32).to_le_bytes());
    out.extend_from_slice(&key_area);
    out.extend_from_slice(&blob_area);
    Ok(out)
}

/// 解析**单个场次**内每个题库的实际抽题数。
///
/// 为什么必须按场次隔离：`picks` 会随本场题库一起加密进该场的 data 段，
/// 客户端抽题时读的就是这一份。早期实现用全局 `exam_id → pick` 映射并取
/// `max()`，导致场次 1 设 2 题被场次 2 的 5 题覆盖（实测：清单要求 2 题，
/// 实际抽出 5 题）。`validate_sessions` 已保证同一场次内不重复引用题库，
/// 因此这里 `insert` 不会互相覆盖。
fn resolve_session_picks(
    banks: &[crate::model::ExamBank],
    session: &Session,
) -> Result<BTreeMap<String, u32>, ContainerError> {
    if session.students.is_empty() {
        return Err(ContainerError::Format(format!(
            "场次 {} 的学号名单为空",
            session.session_id
        )));
    }
    let mut picks: BTreeMap<String, u32> = BTreeMap::new();
    for e in &session.exams {
        let bank = match_bank(&e.file, banks).ok_or_else(|| {
            ContainerError::Format(format!(
                "场次 {} 引用的题库 {} 匹配不到",
                session.session_id, e.file
            ))
        })?;
        let effective = e.pick.unwrap_or(bank.pick);
        if effective as usize > bank.questions.len() {
            return Err(ContainerError::Format(format!(
                "场次 {} 的题库 {} 要求抽 {effective} 题，但只有 {} 题",
                session.session_id,
                bank.exam_id,
                bank.questions.len()
            )));
        }
        picks.insert(bank.exam_id.clone(), effective);
    }
    Ok(picks)
}

/// 构建期校验：这些错误必须在打包时炸，绝不能留到考场。
///
/// 与 `resolve_session_picks` 分开：这里的检查与题库无关，只依赖清单自身。
pub fn validate_sessions(sessions: &[Session]) -> Result<(), ContainerError> {
    validate_sessions_impl(sessions, true)
}

/// 同 [`validate_sessions`]，但可选是否打印警告。
///
/// `build_payload` 内部调用时传 `false`——因为 builder 层已经调用过一次
/// 并打印过警告了，重复打印会让管理员困惑。
pub fn validate_sessions_silent(sessions: &[Session]) -> Result<(), ContainerError> {
    validate_sessions_impl(sessions, false)
}

fn validate_sessions_impl(sessions: &[Session], emit_warnings: bool) -> Result<(), ContainerError> {
    if sessions.is_empty() {
        return Err(ContainerError::Format("清单里没有任何场次".into()));
    }
    let mut seen_ids = std::collections::HashSet::new();
    for s in sessions {
        if !seen_ids.insert(s.session_id) {
            return Err(ContainerError::Format(format!(
                "场次编号 {} 重复",
                s.session_id
            )));
        }
        // 时间窗倒挂：学生永远解不开，属于"考场才炸"的错误。
        match (
            crate::timegate::parse_rfc3339(&s.start),
            crate::timegate::parse_rfc3339(&s.end),
        ) {
            (Some(a), Some(b)) => {
                if a >= b {
                    return Err(ContainerError::Format(format!(
                        "场次 {} 的开始时间必须早于结束时间（{} >= {}）",
                        s.session_id, s.start, s.end
                    )));
                }
            }
            _ => {
                return Err(ContainerError::Format(format!(
                    "场次 {} 的时间无法解析为 RFC3339：start={:?} end={:?}",
                    s.session_id, s.start, s.end
                )));
            }
        }
        // 学号校验：必须是正整数。
        //
        // 上限**不设 INT_MAX**——真实学号常为 12 位（如 202312345678），
        // 早已超出 i32。内部用 u64，足以容纳任何真实学号。
        let mut seen_students = std::collections::HashSet::new();
        for &sid in &s.students {
            if sid == 0 {
                return Err(ContainerError::Format(format!(
                    "场次 {} 的学号为 0（学号必须是正整数）",
                    s.session_id
                )));
            }
            if !seen_students.insert(sid) {
                return Err(ContainerError::Format(format!(
                    "场次 {} 的学号 {} 重复出现",
                    s.session_id, sid
                )));
            }
        }
        // 同一场次不得重复引用同一题库：否则试卷里会出现两个同名科目，
        // 且抽题数按 max 取值，学生看到的是一份重复的卷子。
        let mut seen_exams = std::collections::HashSet::new();
        for e in &s.exams {
            let key = resolve_exam_key(&e.file);
            if !seen_exams.insert(key.clone()) {
                return Err(ContainerError::Format(format!(
                    "场次 {} 重复引用题库 {}（同一场次只能考一次）",
                    s.session_id, e.file
                )));
            }
        }
        if s.exams.is_empty() {
            return Err(ContainerError::Format(format!(
                "场次 {} 没有任何考试科目",
                s.session_id
            )));
        }
    }

    // 时间窗重叠处理：
    //
    // - **同一学生**同时落在两个重叠场次里 → **报错**。客户端只能取
    //   session_id 最小的一场，学生既无法选择也不知道，这是配置歧义，
    //   不该让学生承担。
    // - 重叠但学生无交集 → 仅警告。这是合法用法（同一场次分两个班）。
    check_student_overlap(sessions)?;
    if emit_warnings {
        warn_overlapping_windows(sessions);
    }

    Ok(())
}

/// 检测"同一学生同时在两个重叠场次"的歧义配置。
fn check_student_overlap(sessions: &[Session]) -> Result<(), ContainerError> {
    for i in 0..sessions.len() {
        for j in (i + 1)..sessions.len() {
            let (a, b) = (&sessions[i], &sessions[j]);
            let (Some(a_start), Some(a_end)) = (
                crate::timegate::parse_rfc3339(&a.start),
                crate::timegate::parse_rfc3339(&a.end),
            ) else {
                continue;
            };
            let (Some(b_start), Some(b_end)) = (
                crate::timegate::parse_rfc3339(&b.start),
                crate::timegate::parse_rfc3339(&b.end),
            ) else {
                continue;
            };
            if !(a_start < b_end && b_start < a_end) {
                continue; // 时间窗不重叠
            }
            // 用 HashSet 把 O(m²) 的 `Vec::contains` 降到 O(m)。
            // 20 场次 × 500 学生时，原实现约 4750 万次比较（实测 ~100ms），
            // 改用 HashSet 后降到 190 万次比较。
            let b_set: std::collections::HashSet<u64> = b.students.iter().copied().collect();
            let common: Vec<u64> = a
                .students
                .iter()
                .filter(|s| b_set.contains(s))
                .copied()
                .collect();
            if !common.is_empty() {
                return Err(ContainerError::Format(format!(
                    "场次 {} 与 {} 时间窗重叠，且学生 {:?} 同时在两场。\n\
                     客户端无法判断该进哪一场。请调整时间窗，或把这些学生从其中一场移除。",
                    a.session_id, b.session_id, common
                )));
            }
        }
    }
    Ok(())
}

/// 检测并警告时间窗重叠的场次对。
fn warn_overlapping_windows(sessions: &[Session]) {
    for i in 0..sessions.len() {
        for j in (i + 1)..sessions.len() {
            let a = &sessions[i];
            let b = &sessions[j];
            let (Some(a_start), Some(a_end)) = (
                crate::timegate::parse_rfc3339(&a.start),
                crate::timegate::parse_rfc3339(&a.end),
            ) else {
                continue;
            };
            let (Some(b_start), Some(b_end)) = (
                crate::timegate::parse_rfc3339(&b.start),
                crate::timegate::parse_rfc3339(&b.end),
            ) else {
                continue;
            };
            // 两个闭区间相交：a_start < b_end && b_start < a_end
            if a_start < b_end && b_start < a_end {
                eprintln!(
                    "⚠ 警告：场次 {}（{} ~ {}）与场次 {}（{} ~ {}）时间窗重叠。\n\
                     学生若同时在这两场，客户端会取 session_id 较小的一场。",
                    a.session_id, a.start, a.end, b.session_id, b.start, b.end
                );
            }
        }
    }
}

// 时间解析统一到 `crate::timegate::parse_rfc3339`。
// 这里曾有一份逐字节相同的副本（含 days_from_civil），属于复制粘贴冗余，
// 已删除——两处实现若将来不同步会引发隐蔽 bug。

/// 由 master 派生 **header 加密密钥**（K_manifest）。
///
/// 这是 master 在容器格式中的**唯一用途**：题库与信封都用每场独立的
/// `session_secret` 加密，与 master 无关。
///
/// 因此"让学生端免输 master"只需嵌入这一个 32 字节密钥：
/// - 泄漏后果：攻击者可读场次时间与人数，但**拿不到任何题目**；
/// - 对比现状：现在管理员要把 master 交给每个学生，危害大得多。
///
/// 注意：salt 必须为空串。若用真实 salt 派生，客户端会陷入
/// "要 salt 才能解 header、要解 header 才能拿 salt"的死锁。
pub fn manifest_key(master: &[u8; 32]) -> [u8; 32] {
    *crypto::hkdf_derive(master, b"", "manifest")
}

/// 给 exe 追加 payload，生成自解压学生端。
pub fn append_payload_to_exe(stub_bytes: &[u8], payload: &[u8]) -> Result<Vec<u8>, ContainerError> {
    let mut out = stub_bytes.to_vec();
    out.extend_from_slice(payload);
    out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    out.extend_from_slice(MAGIC_TAIL);
    Ok(out)
}

/// 把清单里的 `file` 归一成候选键（仅文件名去 `.json`）。
pub fn resolve_exam_key(file: &str) -> String {
    let base = file.rsplit('/').next().unwrap_or(file);
    let stem = base.strip_suffix(".json").unwrap_or(base);
    stem.replace(['/', '_'], "-")
}

/// 在题库列表里按清单的 `file` 找到对应题库。
///
/// 匹配优先级：精确 stem → 完整路径归一 → 下划线归一。
/// 都匹配不到返回 `None`，由调用方报错——**绝不静默回退**。
pub fn match_bank<'a>(
    file: &str,
    banks: &'a [crate::model::ExamBank],
) -> Option<&'a crate::model::ExamBank> {
    let base = file.rsplit('/').next().unwrap_or(file);
    let stem = base.strip_suffix(".json").unwrap_or(base);
    let full_key = file
        .strip_suffix(".json")
        .unwrap_or(file)
        .replace(['/', '_'], "-");

    banks
        .iter()
        .find(|b| b.exam_id == stem)
        .or_else(|| banks.iter().find(|b| b.exam_id == full_key))
        .or_else(|| banks.iter().find(|b| b.exam_id == stem.replace('_', "-")))
}

/// 从自解压 exe 字节中提取 payload。
///
/// 所有长度计算都做边界检查：损坏/截断的文件返回错误而不是 panic。
pub fn extract_payload(exe_bytes: &[u8]) -> Result<&[u8], ContainerError> {
    const FOOTER: usize = 16; // payload_len(8) + magic_tail(8)
    if exe_bytes.len() < FOOTER + MAGIC.len() {
        return Err(ContainerError::Format("文件过小".into()));
    }
    let tail = &exe_bytes[exe_bytes.len() - 8..];
    if tail != MAGIC_TAIL {
        return Err(ContainerError::Format("缺少尾部魔数，非自解压包".into()));
    }
    let len_bytes = &exe_bytes[exe_bytes.len() - 16..exe_bytes.len() - 8];
    let payload_len = u64::from_le_bytes(len_bytes.try_into().expect("8 字节切片")) as usize;

    let available = exe_bytes.len() - FOOTER;
    if payload_len > available {
        return Err(ContainerError::Format(format!(
            "payload 长度 {payload_len} 超出文件可用范围 {available}（文件损坏或被截断）"
        )));
    }
    let start = available - payload_len;
    if &exe_bytes[start..start + MAGIC.len()] != MAGIC {
        return Err(ContainerError::Format("payload 魔数不匹配".into()));
    }
    Ok(&exe_bytes[start..start + payload_len])
}

/// 解析 payload 的 header（需要 K_manifest）。
/// 从 payload 里取出内嵌的 K_manifest。
///
/// 返回：
/// - `Ok(Some(k))`：包内嵌了密钥，学生端可直接用；
/// - `Ok(None)`：包未内嵌密钥（`--no-embed-key` 打的包），学生需提供 master；
/// - `Err(_)`：payload 本身损坏。
///
/// 这是学生端**双击即可运行**的关键：密钥随包分发，不需要教师重新编译 client。
pub fn embedded_k_manifest(payload: &[u8]) -> Result<Option<[u8; 32]>, ContainerError> {
    if payload.len() < 20 || &payload[..8] != MAGIC {
        return Err(ContainerError::Format("payload 魔数错误".into()));
    }
    let version = u32::from_le_bytes(payload[8..12].try_into().expect("4 字节"));
    if version != VERSION {
        return Err(ContainerError::Format(format!(
            "不支持的版本 {version}（当前支持 {VERSION}）"
        )));
    }
    let header_len = u32::from_le_bytes(payload[12..16].try_into().expect("4 字节")) as usize;
    let key_off = 16 + header_len;
    if payload.len() < key_off + 4 {
        return Err(ContainerError::Format("header 长度越界".into()));
    }
    let key_len =
        u32::from_le_bytes(payload[key_off..key_off + 4].try_into().expect("4 字节")) as usize;
    if key_len == 0 {
        return Ok(None);
    }
    let end = key_off + 4 + key_len;
    if end > payload.len() {
        return Err(ContainerError::Format("key 区长度越界".into()));
    }
    Ok(read_key_area(&payload[key_off + 4..end]))
}

pub fn parse_header<'a>(
    payload: &'a [u8],
    k_manifest: &[u8; 32],
) -> Result<(Header, &'a [u8]), ContainerError> {
    if payload.len() < 16 || &payload[..8] != MAGIC {
        return Err(ContainerError::Format("payload 魔数错误".into()));
    }
    let version = u32::from_le_bytes(payload[8..12].try_into().expect("4 字节"));
    if version != VERSION {
        return Err(ContainerError::Format(format!(
            "不支持的版本 {version}（当前支持 {VERSION}）"
        )));
    }
    let header_len = u32::from_le_bytes(payload[12..16].try_into().expect("4 字节")) as usize;
    if payload.len() < 20 + header_len {
        return Err(ContainerError::Format("header/key 长度越界".into()));
    }
    let header_ct = &payload[16..16 + header_len];
    let key_off = 16 + header_len;
    let key_len =
        u32::from_le_bytes(payload[key_off..key_off + 4].try_into().expect("4 字节")) as usize;
    let blob_off = key_off + 4 + key_len;
    if blob_off > payload.len() {
        return Err(ContainerError::Format("key 区长度越界".into()));
    }
    let blob_area = &payload[blob_off..];

    let header_plain = crypto::decrypt(k_manifest, header_ct, b"header-v3")?;
    let header: Header = serde_json::from_slice(&header_plain)?;
    Ok((header, blob_area))
}

/// 用学号尝试解锁某个场次。
///
/// 返回 `Ok(None)` 表示该生不在这场；返回 `Err` 表示包损坏或密钥错误。
pub fn try_unlock_session(
    sh: &SessionHeader,
    header: &Header,
    blob_area: &[u8],
    student_id: u64,
    salt: &[u8],
) -> Result<Option<UnlockedSession>, ContainerError> {
    let entry_key = crypto::session_entry_key(student_id, sh.session_id, salt, header.kdf)?;
    let tag = crypto::envelope_tag(&entry_key);

    // 按标签直接定位，只对命中的信封做一次 AEAD 解密。
    let Some(env) = sh.envelopes.iter().find(|e| e.tag == tag) else {
        return Ok(None);
    };
    let Some(payload) = env.open(&entry_key)? else {
        return Ok(None);
    };
    // 双保险：标签碰撞（概率 2^-64）时再校验学号。
    if payload.student_id != student_id {
        return Ok(None);
    }

    let secret_bytes = hex::decode(&payload.session_secret)
        .map_err(|e| ContainerError::Format(format!("session_secret hex: {e}")))?;
    if secret_bytes.len() != 32 {
        return Err(ContainerError::Format("session_secret 长度错误".into()));
    }
    let mut session_secret = [0u8; 32];
    session_secret.copy_from_slice(&secret_bytes);

    // 切片本场题库密文。
    let off = sh.data_off as usize;
    let len = sh.data_len as usize;
    let end = off
        .checked_add(len)
        .ok_or_else(|| ContainerError::Format("data 偏移溢出".into()))?;
    if end > blob_area.len() {
        return Err(ContainerError::Format("data 偏移越界（包损坏）".into()));
    }

    Ok(Some(UnlockedSession {
        session_id: sh.session_id,
        start: sh.start.clone(),
        end: sh.end.clone(),
        exams: payload.exams,
        data_ct: blob_area[off..end].to_vec(),
        session_secret: Zeroizing::new(session_secret),
    }))
}

/// 解密某场次的题库数据。
pub fn decrypt_data(
    session_secret: &[u8; 32],
    salt: &[u8],
    data_ct: &[u8],
) -> Result<PayloadData, ContainerError> {
    let bank_key = crypto::hkdf_derive(session_secret, salt, "bank");
    let plain = crypto::decrypt(&bank_key, data_ct, b"session-data")?;
    let data: PayloadData = serde_json::from_slice(&plain)?;
    Ok(data)
}

fn base64_encode(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn base64_decode(s: &str) -> Result<Vec<u8>, base64::DecodeError> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(s)
}

/// 计算 payload 完整性哈希。
pub fn payload_hash(payload: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(payload))
}

/// 把 `[u8; 8]` 标签序列化为 hex 字符串，便于人工检查与跨语言兼容。
mod tag_serde {
    use serde::{Deserialize, Deserializer, Serializer, de};

    pub fn serialize<S: Serializer>(tag: &[u8; 8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(tag))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 8], D::Error> {
        let s = String::deserialize(d)?;
        let raw = hex::decode(&s).map_err(de::Error::custom)?;
        if raw.len() != 8 {
            return Err(de::Error::custom("tag 必须是 8 字节"));
        }
        let mut out = [0u8; 8];
        out.copy_from_slice(&raw);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ExamBank, Question, SessionExam};

    fn bank(id: &str, n: usize) -> ExamBank {
        ExamBank {
            exam_id: id.into(),
            title: id.into(),
            pick: 5,
            questions: (0..n)
                .map(|i| Question {
                    id: format!("{id}-q{i}"),
                    qtype: "single".into(),
                    stem: "s".into(),
                    options: vec!["A".into()],
                    answer: Some("A".into()),
                    score: 2,
                })
                .collect(),
        }
    }

    fn session(id: u32, students: Vec<u64>) -> Session {
        Session {
            session_id: id,
            start: "2026-12-20T09:00:00+08:00".into(),
            end: "2026-12-20T11:00:00+08:00".into(),
            students,
            exams: vec![SessionExam {
                file: "math.json".into(),
                pick: None,
            }],
        }
    }

    fn master() -> [u8; 32] {
        [0xAB; 32]
    }

    fn kdf() -> KdfParams {
        KdfParams::cheap_for_tests()
    }

    const SALT: &[u8] = b"test-salt-1234";

    fn k_manifest() -> [u8; 32] {
        *crypto::hkdf_derive(&master(), b"", "manifest")
    }

    fn unlock(
        payload: &[u8],
        sid: u64,
        session_id: u32,
    ) -> Result<Option<UnlockedSession>, ContainerError> {
        let (header, blob) = parse_header(payload, &k_manifest())?;
        let sh = header
            .sessions
            .iter()
            .find(|s| s.session_id == session_id)
            .ok_or_else(|| ContainerError::Format("场次不存在".into()))?;
        try_unlock_session(sh, &header, blob, sid, SALT)
    }

    #[test]
    fn build_and_unlock_roundtrip() {
        let banks = vec![bank("math", 50)];
        let sessions = vec![session(1, vec![2021001, 2021002])];
        let payload = build_payload(&master(), &banks, &sessions, SALT, kdf(), true).unwrap();

        let unlocked = unlock(&payload, 2021001, 1).unwrap().expect("应解锁成功");
        assert_eq!(unlocked.session_id, 1);
        let data = decrypt_data(&unlocked.session_secret, SALT, &unlocked.data_ct).unwrap();
        assert_eq!(data.banks[0].questions.len(), 50);
        assert!(
            data.banks[0].questions.iter().all(|q| q.answer.is_none()),
            "答案必须被剥离"
        );
        assert_eq!(data.bank_index[0].total, 50);
    }

    #[test]
    fn unknown_student_is_denied() {
        let banks = vec![bank("math", 50)];
        let payload = build_payload(
            &master(),
            &banks,
            &[session(1, vec![2021001])],
            SALT,
            kdf(),
            true,
        )
        .unwrap();
        assert!(unlock(&payload, 9999999, 1).unwrap().is_none());
    }

    /// v3 关键能力：内嵌密钥后，**不需要 master** 也能解出 header。
    ///
    /// 这是"学生双击即可使用"的技术基础。此前教师必须重新编译 client
    /// 才能产出可用的包，而教师没有 Rust 工具链。
    #[test]
    fn embedded_key_allows_unlock_without_master() {
        let banks = vec![bank("math", 20)];
        let sessions = vec![session(1, vec![2021001])];
        let payload = build_payload(&master(), &banks, &sessions, SALT, kdf(), true).unwrap();

        let k = embedded_k_manifest(&payload).unwrap().expect("应内嵌密钥");
        assert_eq!(k, manifest_key(&master()), "内嵌的应是 K_manifest");

        // 用内嵌密钥解 header —— 完全不碰 master
        let (header, _blob) = parse_header(&payload, &k).unwrap();
        assert_eq!(header.sessions.len(), 1);
    }

    /// `embed = false` 时不得写入密钥区。
    #[test]
    fn no_key_area_when_not_embedded() {
        let banks = vec![bank("math", 20)];
        let payload = build_payload(
            &master(),
            &banks,
            &[session(1, vec![1])],
            SALT,
            kdf(),
            false,
        )
        .unwrap();
        assert!(
            embedded_k_manifest(&payload).unwrap().is_none(),
            "未内嵌时不应有密钥区"
        );
    }

    /// 密钥区里不得出现 K_manifest 的 64-hex 明文（掩码后应搜不到）。
    #[test]
    fn key_area_is_obfuscated() {
        let banks = vec![bank("math", 20)];
        let payload =
            build_payload(&master(), &banks, &[session(1, vec![1])], SALT, kdf(), true).unwrap();
        let k = manifest_key(&master());
        assert!(
            !contains_bytes(&payload, hex::encode(k).as_bytes()),
            "payload 中出现了 K_manifest 的 hex 明文"
        );
        assert!(
            !contains_bytes(&payload, &k),
            "payload 中出现了 K_manifest 的原始字节"
        );
    }

    /// 密钥区被篡改时，header 解密必须失败（不能静默用坏密钥）。
    #[test]
    fn tampered_key_area_fails_to_decrypt_header() {
        let banks = vec![bank("math", 20)];
        let mut payload =
            build_payload(&master(), &banks, &[session(1, vec![1])], SALT, kdf(), true).unwrap();
        let k = embedded_k_manifest(&payload).unwrap().unwrap();
        // 翻转密钥区最后一个字节（密钥区紧跟在 header 之后）
        let header_len = u32::from_le_bytes(payload[12..16].try_into().unwrap()) as usize;
        let idx = 16 + header_len + 4 + KEY_AREA_LEN - 1;
        payload[idx] ^= 0xff;
        let bad = embedded_k_manifest(&payload).unwrap().unwrap();
        assert_ne!(bad, k);
        assert!(
            parse_header(&payload, &bad).is_err(),
            "坏密钥必须解不开 header"
        );
    }

    /// **核心安全验收**：payload 里不得出现任何学号的明文。
    #[test]
    fn student_ids_never_appear_in_payload() {
        let students: Vec<u64> = (2021001..2021011).collect();
        let banks = vec![bank("math", 50)];
        let payload = build_payload(
            &master(),
            &banks,
            &[session(1, students.clone())],
            SALT,
            kdf(),
            true,
        )
        .unwrap();

        for sid in &students {
            assert!(
                !contains_bytes(&payload, sid.to_string().as_bytes()),
                "payload 中出现了学号明文 {sid}"
            );
            assert!(
                !contains_bytes(&payload, &sid.to_le_bytes()),
                "payload 中出现了学号 LE 字节 {sid}"
            );
            assert!(
                !contains_bytes(&payload, &sid.to_be_bytes()),
                "payload 中出现了学号 BE 字节 {sid}"
            );
        }
    }

    fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    /// 场次之间必须密码学隔离。
    #[test]
    fn sessions_are_cryptographically_isolated() {
        let banks = vec![bank("math", 30), bank("english", 20)];
        let s1 = Session {
            session_id: 1,
            start: "2026-12-20T09:00:00+08:00".into(),
            end: "2026-12-20T11:00:00+08:00".into(),
            students: vec![1001],
            exams: vec![SessionExam {
                file: "math.json".into(),
                pick: None,
            }],
        };
        let s2 = Session {
            session_id: 2,
            start: "2026-12-20T14:00:00+08:00".into(),
            end: "2026-12-20T16:00:00+08:00".into(),
            students: vec![2002],
            exams: vec![SessionExam {
                file: "english.json".into(),
                pick: None,
            }],
        };
        let payload = build_payload(&master(), &banks, &[s1, s2], SALT, kdf(), true).unwrap();

        let u1 = unlock(&payload, 1001, 1).unwrap().expect("场次1应解锁");
        let u2 = unlock(&payload, 2002, 2).unwrap().expect("场次2应解锁");

        assert_ne!(*u1.session_secret, *u2.session_secret);
        assert!(
            decrypt_data(&u1.session_secret, SALT, &u2.data_ct).is_err(),
            "场次1的密钥竟能解开场次2的题库"
        );
        assert!(unlock(&payload, 1001, 2).unwrap().is_none());
    }

    #[test]
    fn explicit_pick_is_honored() {
        let banks = vec![bank("math", 50)];
        let s = Session {
            session_id: 1,
            start: "2026-12-20T09:00:00+08:00".into(),
            end: "2026-12-20T11:00:00+08:00".into(),
            students: vec![1],
            exams: vec![SessionExam {
                file: "math.json".into(),
                pick: Some(17),
            }],
        };
        let payload = build_payload(&master(), &banks, &[s], SALT, kdf(), true).unwrap();
        let u = unlock(&payload, 1, 1).unwrap().unwrap();
        let data = decrypt_data(&u.session_secret, SALT, &u.data_ct).unwrap();
        assert_eq!(data.pick_for("math", 5), 17);
        assert_eq!(data.bank_index[0].pick, 17);
    }

    #[test]
    fn pick_exceeds_pool_is_build_error() {
        let banks = vec![bank("math", 6)];
        let s = Session {
            session_id: 1,
            start: "2026-12-20T09:00:00+08:00".into(),
            end: "2026-12-20T11:00:00+08:00".into(),
            students: vec![1],
            exams: vec![SessionExam {
                file: "math.json".into(),
                pick: Some(99),
            }],
        };
        assert!(build_payload(&master(), &banks, &[s], SALT, kdf(), true).is_err());
    }

    #[test]
    fn unknown_bank_reference_is_error() {
        let banks = vec![bank("math", 10)];
        let s = Session {
            session_id: 1,
            start: "2026-12-20T09:00:00+08:00".into(),
            end: "2026-12-20T11:00:00+08:00".into(),
            students: vec![1],
            exams: vec![SessionExam {
                file: "nope.json".into(),
                pick: None,
            }],
        };
        assert!(build_payload(&master(), &banks, &[s], SALT, kdf(), true).is_err());
    }

    #[test]
    fn empty_roster_is_build_error() {
        let banks = vec![bank("math", 10)];
        let s = Session {
            session_id: 1,
            start: "2026-12-20T09:00:00+08:00".into(),
            end: "2026-12-20T11:00:00+08:00".into(),
            students: vec![],
            exams: vec![SessionExam {
                file: "math.json".into(),
                pick: None,
            }],
        };
        assert!(build_payload(&master(), &banks, &[s], SALT, kdf(), true).is_err());
    }

    #[test]
    fn append_and_extract() {
        let payload = build_payload(
            &master(),
            &[bank("math", 20)],
            &[session(1, vec![1])],
            SALT,
            kdf(),
            true,
        )
        .unwrap();
        let sfx = append_payload_to_exe(b"FAKE_STUB_BYTES_1234567890", &payload).unwrap();
        assert_eq!(extract_payload(&sfx).unwrap(), payload.as_slice());
    }

    #[test]
    fn wrong_manifest_key_fails() {
        let payload = build_payload(
            &master(),
            &[bank("math", 10)],
            &[session(1, vec![1])],
            SALT,
            kdf(),
            true,
        )
        .unwrap();
        assert!(parse_header(&payload, &[0xCD; 32]).is_err());
    }

    #[test]
    fn corrupt_footer_returns_error() {
        let payload = build_payload(
            &master(),
            &[bank("math", 5)],
            &[session(1, vec![1])],
            SALT,
            kdf(),
            true,
        )
        .unwrap();
        let mut sfx = append_payload_to_exe(b"STUB", &payload).unwrap();
        let n = sfx.len();
        sfx[n - 16..n - 8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(extract_payload(&sfx).is_err(), "超大长度应报错而非 panic");
    }

    #[test]
    fn truncated_file_returns_error() {
        assert!(extract_payload(b"").is_err());
        assert!(extract_payload(b"EXAMTAIL").is_err());
        assert!(extract_payload(b"short").is_err());
    }

    #[test]
    fn tampered_header_returns_error() {
        let mut payload = build_payload(
            &master(),
            &[bank("math", 10)],
            &[session(1, vec![1])],
            SALT,
            kdf(),
            true,
        )
        .unwrap();
        payload[20] ^= 0xFF;
        assert!(parse_header(&payload, &k_manifest()).is_err());
    }

    #[test]
    fn match_bank_prefers_exact_exam_id() {
        let banks = vec![bank("demo-math", 50), bank("math", 10)];
        assert_eq!(
            match_bank("demo/math.json", &banks).unwrap().exam_id,
            "math"
        );
    }

    #[test]
    fn match_bank_falls_back_to_full_path() {
        let banks = vec![bank("demo-math", 50)];
        assert_eq!(
            match_bank("demo/math.json", &banks).unwrap().exam_id,
            "demo-math"
        );
    }

    #[test]
    fn match_bank_returns_none_when_missing() {
        assert!(match_bank("nope.json", &[bank("math", 10)]).is_none());
    }

    /// 回归：同一场次重复引用同一题库必须报错（会产生两份同名试卷）。
    #[test]
    fn rejects_duplicate_exam_in_same_session() {
        use crate::model::SessionExam;
        let s = Session {
            session_id: 1,
            start: "2026-12-20T09:00:00+08:00".into(),
            end: "2026-12-20T11:00:00+08:00".into(),
            students: vec![1],
            exams: vec![
                SessionExam {
                    file: "demo/math.json".into(),
                    pick: Some(2),
                },
                SessionExam {
                    file: "demo/math.json".into(),
                    pick: Some(3),
                },
            ],
        };
        let err = validate_sessions(&[s]).unwrap_err();
        assert!(
            matches!(err, ContainerError::Format(ref m) if m.contains("重复引用")),
            "实际: {err}"
        );
    }

    /// 不同题库的同一场次应当允许。
    #[test]
    fn allows_distinct_exams_in_same_session() {
        use crate::model::SessionExam;
        let s = Session {
            session_id: 1,
            start: "2026-12-20T09:00:00+08:00".into(),
            end: "2026-12-20T11:00:00+08:00".into(),
            students: vec![1],
            exams: vec![
                SessionExam {
                    file: "math.json".into(),
                    pick: None,
                },
                SessionExam {
                    file: "english.json".into(),
                    pick: None,
                },
            ],
        };
        assert!(validate_sessions(&[s]).is_ok());
    }

    /// 场次没有任何科目应当报错。
    #[test]
    fn rejects_session_without_exams() {
        let s = Session {
            session_id: 1,
            start: "2026-12-20T09:00:00+08:00".into(),
            end: "2026-12-20T11:00:00+08:00".into(),
            students: vec![1],
            exams: vec![],
        };
        assert!(validate_sessions(&[s]).is_err());
    }

    /// 时间窗重叠但**学生无交集** → 合法（同一场次分两个班），只警告。
    #[test]
    fn overlapping_windows_without_shared_students_is_ok() {
        use crate::model::SessionExam;
        let mk = |id: u32, start: &str, end: &str, students: Vec<u64>| Session {
            session_id: id,
            start: start.into(),
            end: end.into(),
            students,
            exams: vec![SessionExam {
                file: "math.json".into(),
                pick: None,
            }],
        };
        let a = mk(
            1,
            "2026-12-20T09:00:00+08:00",
            "2026-12-20T11:00:00+08:00",
            vec![1, 2],
        );
        let b = mk(
            2,
            "2026-12-20T10:00:00+08:00",
            "2026-12-20T12:00:00+08:00",
            vec![3, 4],
        );
        assert!(
            validate_sessions(&[a, b]).is_ok(),
            "学生无交集的重叠是合法分班，应只警告"
        );
    }

    /// **P0 回归**：不同场次引用同一题库时，pick 必须互不干扰。
    ///
    /// 早期实现用全局 exam_id→pick 映射并取 max，导致场次 1 设 2 题
    /// 被场次 2 的 5 题覆盖（实测：清单要求 2 题，实际抽出 5 题）。
    #[test]
    fn picks_are_isolated_per_session() {
        use crate::model::SessionExam;
        let banks = vec![bank("math", 50)];
        // 注意：必须用常量 SALT，因为下面的 unlock() 辅助函数内部用的是它。
        let salt = SALT;
        // 场次 1 抽 2 题，场次 2 抽 5 题，引用同一题库。
        // 学生不同、时间窗不重叠，避免触发其他校验。
        let s1 = Session {
            session_id: 1,
            start: "2026-12-20T09:00:00+08:00".into(),
            end: "2026-12-20T11:00:00+08:00".into(),
            students: vec![1001],
            exams: vec![SessionExam {
                file: "math.json".into(),
                pick: Some(2),
            }],
        };
        let s2 = Session {
            session_id: 2,
            start: "2026-12-20T14:00:00+08:00".into(),
            end: "2026-12-20T16:00:00+08:00".into(),
            students: vec![2002],
            exams: vec![SessionExam {
                file: "math.json".into(),
                pick: Some(5),
            }],
        };
        let payload = build_payload(&master(), &banks, &[s1, s2], salt, kdf(), true).unwrap();

        let u1 = unlock(&payload, 1001, 1).unwrap().expect("场次1应解锁");
        let d1 = decrypt_data(&u1.session_secret, salt, &u1.data_ct).unwrap();
        assert_eq!(
            d1.pick_for("math", 0),
            2,
            "场次1 必须抽 2 题，不能被场次2 的 5 题污染"
        );

        let u2 = unlock(&payload, 2002, 2).unwrap().expect("场次2应解锁");
        let d2 = decrypt_data(&u2.session_secret, salt, &u2.data_ct).unwrap();
        assert_eq!(d2.pick_for("math", 0), 5, "场次2 必须抽 5 题");
    }

    /// **回归**：时间窗重叠且**同一学生**在两场 → 必须报错。
    ///
    /// 客户端只能取 session_id 最小的一场，学生既无法选择也不知道，
    /// 这是配置歧义，不该让学生承担。
    #[test]
    fn overlapping_windows_with_shared_student_is_error() {
        use crate::model::SessionExam;
        let mk = |id: u32, start: &str, end: &str| Session {
            session_id: id,
            start: start.into(),
            end: end.into(),
            students: vec![2021001],
            exams: vec![SessionExam {
                file: "math.json".into(),
                pick: None,
            }],
        };
        let a = mk(1, "2026-12-20T09:00:00+08:00", "2026-12-20T11:00:00+08:00");
        let b = mk(2, "2026-12-20T10:00:00+08:00", "2026-12-20T12:00:00+08:00");
        let err = validate_sessions(&[a, b]).unwrap_err();
        assert!(
            matches!(err, ContainerError::Format(ref m) if m.contains("同时在两场")),
            "实际: {err}"
        );
    }

    /// 学号为 0 必须报错（正整数约束）。
    #[test]
    fn rejects_zero_student_id() {
        let s = Session {
            session_id: 1,
            start: "2026-12-20T09:00:00+08:00".into(),
            end: "2026-12-20T11:00:00+08:00".into(),
            students: vec![0],
            exams: vec![crate::model::SessionExam {
                file: "math.json".into(),
                pick: None,
            }],
        };
        let err = validate_sessions(&[s]).unwrap_err();
        assert!(
            matches!(err, ContainerError::Format(ref m) if m.contains("必须是正整数")),
            "实际: {err}"
        );
    }

    /// 学号可以超过 i32::MAX（真实学号常为 12 位）。
    #[test]
    fn allows_student_id_above_i32_max() {
        let s = Session {
            session_id: 1,
            start: "2026-12-20T09:00:00+08:00".into(),
            end: "2026-12-20T11:00:00+08:00".into(),
            students: vec![202312345678],
            exams: vec![crate::model::SessionExam {
                file: "math.json".into(),
                pick: None,
            }],
        };
        assert!(validate_sessions(&[s]).is_ok(), "12 位学号应当被接受");
    }

    #[test]
    fn rejects_reversed_time_window() {
        let s = Session {
            session_id: 1,
            start: "2026-12-20T11:00:00+08:00".into(),
            end: "2026-12-20T09:00:00+08:00".into(),
            students: vec![1],
            exams: vec![SessionExam {
                file: "math.json".into(),
                pick: None,
            }],
        };
        assert!(validate_sessions(&[s]).is_err(), "时间倒挂必须报错");
    }

    #[test]
    fn rejects_duplicate_students() {
        let s = Session {
            session_id: 1,
            start: "2026-12-20T09:00:00+08:00".into(),
            end: "2026-12-20T11:00:00+08:00".into(),
            students: vec![2021001, 2021001],
            exams: vec![SessionExam {
                file: "math.json".into(),
                pick: None,
            }],
        };
        assert!(validate_sessions(&[s]).is_err(), "重复学号必须报错");
    }

    #[test]
    fn rejects_duplicate_session_ids() {
        let mk = |id: u32| Session {
            session_id: id,
            start: "2026-12-20T09:00:00+08:00".into(),
            end: "2026-12-20T11:00:00+08:00".into(),
            students: vec![1],
            exams: vec![SessionExam {
                file: "math.json".into(),
                pick: None,
            }],
        };
        assert!(validate_sessions(&[mk(1), mk(1)]).is_err());
        // 注意：两场用**不同学生**，否则会因时间窗重叠+学生交集而报错，
        // 掩盖这里真正要测的"场次编号重复"。
        let a = Session {
            session_id: 1,
            students: vec![1],
            ..mk(1)
        };
        let b = Session {
            session_id: 2,
            students: vec![2],
            ..mk(1)
        };
        assert!(validate_sessions(&[a, b]).is_ok());
    }

    #[test]
    fn rejects_unparseable_time() {
        let s = Session {
            session_id: 1,
            start: "tomorrow".into(),
            end: "next week".into(),
            students: vec![1],
            exams: vec![SessionExam {
                file: "math.json".into(),
                pick: None,
            }],
        };
        assert!(validate_sessions(&[s]).is_err());
    }

    #[test]
    fn parse_rfc3339_handles_offsets() {
        let a = crate::timegate::parse_rfc3339("2026-12-20T09:00:00+08:00").unwrap();
        let b = crate::timegate::parse_rfc3339("2026-12-20T01:00:00Z").unwrap();
        assert_eq!(a, b);
        assert_eq!(
            crate::timegate::parse_rfc3339("1970-01-01T00:00:00Z"),
            Some(0)
        );
    }

    #[test]
    fn resolve_exam_key_normalizes() {
        assert_eq!(resolve_exam_key("demo/math.json"), "math");
        assert_eq!(resolve_exam_key("math.json"), "math");
    }
}
