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

use zeroize::Zeroizing;

use crate::crypto::{self, CryptoError, KdfParams, TAG_LEN};
use crate::model::Session;

/// payload 起始魔数。
pub const MAGIC: &[u8; 8] = b"EXAMPKG2";
/// 尾部魔数，用于从 exe 末尾定位 payload。
pub const MAGIC_TAIL: &[u8; 8] = b"EXAMTAIL";
/// 当前格式版本。
pub const VERSION: u32 = 2;

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
pub fn build_payload(
    master: &[u8; 32],
    banks: &[crate::model::ExamBank],
    sessions: &[Session],
    salt: &[u8],
    kdf: KdfParams,
) -> Result<Vec<u8>, ContainerError> {
    // 1. 剥离答案。
    let mut clean_banks = banks.to_vec();
    for bank in &mut clean_banks {
        for q in &mut bank.questions {
            q.answer = None;
        }
    }

    // 2. 清单自身校验（时间窗、重复学号、场次编号）+ 抽题数解析。
    validate_sessions(sessions)?;
    let picks = resolve_picks(&clean_banks, sessions)?;

    // 3. 逐场生成：会话密钥 → 题库密文 → 每生信封。
    let mut blob_area: Vec<u8> = Vec::new();
    let mut session_headers = Vec::with_capacity(sessions.len());

    for s in sessions {
        let session_secret = crypto::random_key();
        let bank_key = crypto::hkdf_derive(&session_secret, salt, "bank");

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
            picks: picks.clone(),
            bank_index: session_index,
        };
        let data_plain = serde_json::to_vec(&data)?;
        let data_ct = crypto::encrypt(&bank_key, &data_plain, b"session-data")?;

        let data_off = blob_area.len() as u32;
        let data_len = data_ct.len() as u32;
        blob_area.extend_from_slice(&data_ct);

        // 每生一个信封。
        let mut envelopes = Vec::with_capacity(s.students.len());
        for &sid in &s.students {
            let entry_key = crypto::session_entry_key(sid, s.session_id, salt, kdf)?;
            let inner = EntryPayload {
                student_id: sid,
                session_secret: hex::encode(*session_secret),
                exams: s.exams.clone(),
            };
            let inner_plain = serde_json::to_vec(&inner)?;
            let blob = crypto::encrypt(&entry_key, &inner_plain, b"envelope-v2")?;
            envelopes.push(Envelope {
                tag: crypto::envelope_tag(&entry_key),
                blob: base64_encode(&blob),
            });
        }

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
    let header_ct = crypto::encrypt(&k_manifest, &header_plain, b"header-v2")?;

    // 5. 组装 payload。
    let mut out = Vec::with_capacity(16 + header_ct.len() + blob_area.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(header_ct.len() as u32).to_le_bytes());
    out.extend_from_slice(&header_ct);
    out.extend_from_slice(&blob_area);
    Ok(out)
}

/// 解析每个题库的实际抽题数（清单值优先，多场取最大值）。
fn resolve_picks(
    banks: &[crate::model::ExamBank],
    sessions: &[Session],
) -> Result<BTreeMap<String, u32>, ContainerError> {
    let mut picks: BTreeMap<String, u32> = BTreeMap::new();
    for s in sessions {
        if s.students.is_empty() {
            return Err(ContainerError::Format(format!(
                "场次 {} 的学号名单为空",
                s.session_id
            )));
        }
        for e in &s.exams {
            let bank = match_bank(&e.file, banks).ok_or_else(|| {
                ContainerError::Format(format!(
                    "场次 {} 引用的题库 {} 匹配不到",
                    s.session_id, e.file
                ))
            })?;
            let effective = e.pick.unwrap_or(bank.pick);
            if effective as usize > bank.questions.len() {
                return Err(ContainerError::Format(format!(
                    "场次 {} 的题库 {} 要求抽 {effective} 题，但只有 {} 题",
                    s.session_id,
                    bank.exam_id,
                    bank.questions.len()
                )));
            }
            let entry = picks.entry(bank.exam_id.clone()).or_insert(effective);
            *entry = (*entry).max(effective);
        }
    }
    Ok(picks)
}

/// 构建期校验：这些错误必须在打包时炸，绝不能留到考场。
///
/// 与 `resolve_picks` 分开：这里的检查与题库无关，只依赖清单自身。
pub fn validate_sessions(sessions: &[Session]) -> Result<(), ContainerError> {
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
        // 重复学号：会让信封数量虚高，也说明清单有误。
        let mut seen_students = std::collections::HashSet::new();
        for &sid in &s.students {
            if !seen_students.insert(sid) {
                return Err(ContainerError::Format(format!(
                    "场次 {} 的学号 {} 重复出现",
                    s.session_id, sid
                )));
            }
        }
    }
    Ok(())
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
    if payload.len() < 16 + header_len {
        return Err(ContainerError::Format("header 长度越界".into()));
    }
    let header_ct = &payload[16..16 + header_len];
    let blob_area = &payload[16 + header_len..];

    let header_plain = crypto::decrypt(k_manifest, header_ct, b"header-v2")?;
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
        let payload = build_payload(&master(), &banks, &sessions, SALT, kdf()).unwrap();

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
        let payload =
            build_payload(&master(), &banks, &[session(1, vec![2021001])], SALT, kdf()).unwrap();
        assert!(unlock(&payload, 9999999, 1).unwrap().is_none());
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
        let payload = build_payload(&master(), &banks, &[s1, s2], SALT, kdf()).unwrap();

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
        let payload = build_payload(&master(), &banks, &[s], SALT, kdf()).unwrap();
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
        assert!(build_payload(&master(), &banks, &[s], SALT, kdf()).is_err());
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
        assert!(build_payload(&master(), &banks, &[s], SALT, kdf()).is_err());
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
        assert!(build_payload(&master(), &banks, &[s], SALT, kdf()).is_err());
    }

    #[test]
    fn append_and_extract() {
        let payload = build_payload(
            &master(),
            &[bank("math", 20)],
            &[session(1, vec![1])],
            SALT,
            kdf(),
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
        assert!(validate_sessions(&[mk(1), mk(2)]).is_ok());
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
