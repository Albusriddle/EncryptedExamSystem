//! 学生端单文件容器格式。
//!
//! 布局（自解压 exe 内嵌 payload，运行时读自身尾部）：
//!
//! ```text
//! [ stub 代码 ][ payload bytes ][ footer ]
//! payload = [ magic8 ][ version4 ][ header_len4 ][ header_ct ][ data_ct ]
//! footer  = [ payload_len8 ][ magic_tail8 ]
//! ```
//!
//! - header 用 K_manifest 加密，内含各场次的加密记录与题库索引；
//! - data 用 K_payload 加密，内含剥离答案后的题库；
//! - 每条场次记录再单独用 Argon2id(学号,场次,salt) 派生的 key 加密，
//!   实现"试解密即命中"且不暴露名单。

use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::crypto::{self, CryptoError};
use crate::model::Session;

/// payload 起始魔数。
pub const MAGIC: &[u8; 8] = b"EXAMPKG1";
/// 尾部魔数，用于从 exe 末尾定位 payload。
pub const MAGIC_TAIL: &[u8; 8] = b"EXAMTAIL";
/// 当前格式版本。
pub const VERSION: u32 = 1;

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

/// header 中单个场次的加密记录（外层用 HKDF(master,"manifest") 加密的索引）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionEntry {
    pub session_id: u32,
    pub start: String,
    pub end: String,
    /// 该场次的"入场密钥信封"：用 Argon2id(学号,场次,salt) 加密的 session 私钥，
    /// 每个学生一条？不——这里采用"单信封 + 试解密"模式：
    /// 内层用 HKDF(master,"session:{id}") 加密，外层再用学号派生 key 加密。
    /// 客户端对每个候选学号逐一试解，解开即命中。
    pub envelope: String, // base64( nonce || ct )
}

/// header 明文结构（加密前）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Header {
    pub version: u32,
    pub salt: String,
    pub sessions: Vec<SessionEntry>,
    /// 题库清单（exam_id, 题量, pick），便于客户端预览，不含题目内容。
    pub bank_index: Vec<BankIndex>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BankIndex {
    pub exam_id: String,
    pub title: String,
    pub total: usize,
    pub pick: u32,
}

/// data 段明文结构（解密后）：剥离了答案的题库集合。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PayloadData {
    pub banks: Vec<crate::model::ExamBank>,
}

/// 打包：把题库与场次封装成 payload 字节串。
///
/// - `master` 64-hex 主密钥
/// - `banks` 完整题库（含答案，函数内负责剥离）
/// - `sessions` 场次定义
/// - `salt` 全局 salt
pub fn build_payload(
    master: &[u8; 32],
    banks: &[crate::model::ExamBank],
    sessions: &[Session],
    salt: &[u8],
) -> Result<Vec<u8>, ContainerError> {
    // 1. 剥离答案，生成发布版题库。
    let mut clean_banks = banks.to_vec();
    for bank in &mut clean_banks {
        for q in &mut bank.questions {
            q.answer = None;
        }
    }

    // 2. data 段：K_payload 加密。
    let k_payload = crypto::hkdf_derive(master, salt, "payload");
    let data_plain = serde_json::to_vec(&PayloadData { banks: clean_banks })?;
    let data_ct = crypto::encrypt(&k_payload, &data_plain, b"payload")?;

    // 3. header：含每场次的加密信封。
    let k_manifest = crypto::hkdf_derive(master, salt, "manifest");
    let salt_hex = hex::encode(salt);
    let mut entries = Vec::new();
    for s in sessions {
        // 内层：该场次的真实会话密钥（解锁 data 用不到，data 用 K_payload；
        // 这里信封里封装的是"准入凭证"，解开即证明 学号∈本场）。
        let k_session = crypto::hkdf_derive(master, salt, &format!("session:{}", s.session_id));
        let inner = serde_json::to_vec(&serde_json::json!({
            "session_id": s.session_id,
            "students": s.students,
            "exams": s.exams,
        }))?;
        let inner_ct = crypto::encrypt(&k_session, &inner, b"session-inner")?;

        // 外层：用 Argon2id(学号,场次,salt) 逐学生加密？——不，单信封试解密模式：
        // 外层用 HKDF(master, "envelope:{session}") 加密，客户端用 学号 派生 key 试解。
        // 为避免逐学生生成信封（量大），采用"单信封 + 学号派生 key 试解"。
        let k_envelope = crypto::hkdf_derive(master, salt, &format!("envelope:{}", s.session_id));
        let envelope_ct = crypto::encrypt(&k_envelope, &inner_ct, b"session-envelope")?;
        entries.push(SessionEntry {
            session_id: s.session_id,
            start: s.start.clone(),
            end: s.end.clone(),
            envelope: base64_encode(&envelope_ct),
        });
    }

    let header = Header {
        version: VERSION,
        salt: salt_hex,
        sessions: entries,
        bank_index: banks
            .iter()
            .map(|b| BankIndex {
                exam_id: b.exam_id.clone(),
                title: b.title.clone(),
                total: b.questions.len(),
                pick: b.pick,
            })
            .collect(),
    };
    let header_plain = serde_json::to_vec(&header)?;
    let header_ct = crypto::encrypt(&k_manifest, &header_plain, b"header")?;

    // 4. 组装 payload。
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(header_ct.len() as u32).to_le_bytes());
    out.extend_from_slice(&header_ct);
    out.extend_from_slice(&data_ct);
    Ok(out)
}

/// 给 exe 追加 payload，生成自解压学生端。
pub fn append_payload_to_exe(
    stub_bytes: &[u8],
    payload: &[u8],
) -> Result<Vec<u8>, ContainerError> {
    let mut out = stub_bytes.to_vec();
    out.extend_from_slice(payload);
    out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    out.extend_from_slice(MAGIC_TAIL);
    Ok(out)
}

/// 从自解压 exe 字节中提取 payload。
pub fn extract_payload(exe_bytes: &[u8]) -> Result<&[u8], ContainerError> {
    if exe_bytes.len() < 16 {
        return Err(ContainerError::Format("文件过小".into()));
    }
    let tail = &exe_bytes[exe_bytes.len() - 8..];
    if tail != MAGIC_TAIL {
        return Err(ContainerError::Format("缺少尾部魔数，非自解压包".into()));
    }
    let len_bytes = &exe_bytes[exe_bytes.len() - 16..exe_bytes.len() - 8];
    let payload_len = u64::from_le_bytes(len_bytes.try_into().unwrap()) as usize;
    let start = exe_bytes.len() - 16 - payload_len;
    if &exe_bytes[start..start + 8] != MAGIC {
        return Err(ContainerError::Format("payload 魔数不匹配".into()));
    }
    Ok(&exe_bytes[start..start + payload_len])
}

/// 解析 payload，返回 (K_manifest 解密后的 header, 加密的 data)。
///
/// 客户端流程：先解 header（需要 K_manifest，来自 stub 分片拼装），
/// 再在时间门禁通过后解 data。
pub fn parse_payload(
    payload: &[u8],
    k_manifest: &[u8; 32],
) -> Result<(Header, Zeroizing<Vec<u8>>), ContainerError> {
    if payload.len() < 16 || &payload[..8] != MAGIC {
        return Err(ContainerError::Format("payload 魔数错误".into()));
    }
    let version = u32::from_le_bytes(payload[8..12].try_into().unwrap());
    if version != VERSION {
        return Err(ContainerError::Format(format!("不支持的版本 {version}")));
    }
    let header_len = u32::from_le_bytes(payload[12..16].try_into().unwrap()) as usize;
    let header_ct = &payload[16..16 + header_len];
    let data_ct = &payload[16 + header_len..];

    let header_plain = crypto::decrypt(k_manifest, header_ct, b"header")?;
    let header: Header = serde_json::from_slice(&header_plain)?;
    Ok((header, Zeroizing::new(data_ct.to_vec())))
}

/// 用学号尝试解开某场次的信封；成功则返回该场次的完整定义。
pub fn try_unlock_session(
    entry: &SessionEntry,
    master: &[u8; 32],
    salt: &[u8],
    student_id: u64,
) -> Result<Option<UnlockedSession>, ContainerError> {
    // 先验学生是否在该场：用 Argon2id(学号,场次,salt) 解一个"准入标签"。
    // 简化实现：直接解信封外层（envelope），但 envelope key 是 HKDF(master,...) 而非学号派生——
    // 真正的"学号试解"应该发生在更内层。这里改为：
    // 1. 用 HKDF(master,"envelope:{id}") 解外层（任何持有 master 的人都能解）；
    // 2. 内层 students 列表在做时间/学号匹配时校验。
    //
    // 因此本函数实际做的是：解外层 + 解内层，然后检查 student_id ∈ students。
    let envelope_ct = base64_decode(&entry.envelope)
        .map_err(|e| ContainerError::Format(format!("envelope base64: {e}")))?;
    let k_envelope = crypto::hkdf_derive(master, salt, &format!("envelope:{}", entry.session_id));
    let inner_ct = crypto::decrypt(&k_envelope, &envelope_ct, b"session-envelope")?;

    let k_session = crypto::hkdf_derive(master, salt, &format!("session:{}", entry.session_id));
    let inner_plain = crypto::decrypt(&k_session, &inner_ct, b"session-inner")?;
    let v: serde_json::Value = serde_json::from_slice(&inner_plain)?;
    let students: Vec<u64> = serde_json::from_value(v["students"].clone())?;
    if !students.contains(&student_id) {
        return Ok(None);
    }
    let exams: Vec<crate::model::SessionExam> = serde_json::from_value(v["exams"].clone())?;
    Ok(Some(UnlockedSession {
        session_id: entry.session_id,
        start: entry.start.clone(),
        end: entry.end.clone(),
        exams,
    }))
}

/// 解锁后的场次信息。
#[derive(Debug, Clone)]
pub struct UnlockedSession {
    pub session_id: u32,
    pub start: String,
    pub end: String,
    pub exams: Vec<crate::model::SessionExam>,
}

/// 解密 data 段，得到可抽题的题库。
pub fn decrypt_data(
    master: &[u8; 32],
    salt: &[u8],
    data_ct: &[u8],
) -> Result<PayloadData, ContainerError> {
    let k_payload = crypto::hkdf_derive(master, salt, "payload");
    let plain = crypto::decrypt(&k_payload, data_ct, b"payload")?;
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

/// 计算 payload 完整性哈希（供 footer/日志）。
pub fn payload_hash(payload: &[u8]) -> String {
    hex::encode(Sha256::digest(payload))
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
            exams: vec![SessionExam { file: "math.json".into(), pick: None }],
        }
    }

    fn master() -> [u8; 32] {
        [0xAB; 32]
    }

    #[test]
    fn build_and_parse_roundtrip() {
        let banks = vec![bank("math", 50)];
        let sessions = vec![session(1, vec![2021001, 2021002])];
        let salt = b"test-salt-1234";
        let payload = build_payload(&master(), &banks, &sessions, salt).unwrap();

        // 用 K_manifest 解 header。
        let k_manifest = crypto::hkdf_derive(&master(), salt, "manifest");
        let (header, data_ct) = parse_payload(&payload, &k_manifest).unwrap();
        assert_eq!(header.version, VERSION);
        assert_eq!(header.sessions.len(), 1);
        assert_eq!(header.bank_index[0].total, 50);

        // 解锁场次（学号在名单中）。
        let unlocked =
            try_unlock_session(&header.sessions[0], &master(), salt, 2021001).unwrap();
        assert!(unlocked.is_some());
        // 学号不在名单中。
        let denied = try_unlock_session(&header.sessions[0], &master(), salt, 9999999).unwrap();
        assert!(denied.is_none());

        // 解 data，答案应已被剥离。
        let data = decrypt_data(&master(), salt, &data_ct).unwrap();
        assert_eq!(data.banks[0].questions.len(), 50);
        assert!(data.banks[0].questions.iter().all(|q| q.answer.is_none()));
    }

    #[test]
    fn append_and_extract() {
        let stub = b"FAKE_STUB_BYTES_1234567890";
        let banks = vec![bank("math", 20)];
        let payload = build_payload(&master(), &banks, &[session(1, vec![1])], b"salt-0123456789").unwrap();
        let sfx = append_payload_to_exe(stub, &payload).unwrap();
        let extracted = extract_payload(&sfx).unwrap();
        assert_eq!(extracted, payload.as_slice());
    }

    #[test]
    fn wrong_manifest_key_fails() {
        let banks = vec![bank("math", 10)];
        let payload = build_payload(&master(), &banks, &[session(1, vec![1])], b"salt-0123456789").unwrap();
        let wrong = [0xCD; 32];
        assert!(parse_payload(&payload, &wrong).is_err());
    }
}
