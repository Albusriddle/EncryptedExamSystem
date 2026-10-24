//! 答题卡：数据模型、序列化、加密封装。
//!
//! ## 为什么放在 common
//!
//! 学生端负责**写**、builder 负责**读**（教师校验工具），两端必须对同一份
//! 字节格式达成一致。放在 common 里只有一份实现，改格式时不会漏掉一边。
//!
//! ## 安全边界（如实声明）
//!
//! 密钥由 `K_manifest` 派生，而 `K_manifest` 就嵌在学生端 exe 里——学生能
//! 还原它，因此**加密只能挡"随手改"，挡不住有心人**。这套机制真正保证的是：
//!
//! 1. 磁盘上没有明文答题卡；
//! 2. 学号/场次/试卷指纹被写进 AAD，改任一项都会解密失败；
//! 3. 教师侧可以用 master 解开并核对。
//!
//! 不保证：学生会伪造自己的答案、同包学生互相读取对方的答题卡。
//! 真正的防作弊只能靠现场监考或在线提交。

use std::path::Path;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::crypto;
use crate::picker::Paper;

/// 文件魔数（8 字节）。
pub const MAGIC: &[u8; 8] = b"EESANS01";
/// 文件版本。
pub const VERSION: u32 = 1;
/// 指纹长度（blake3 hex）。
pub const FP_LEN: usize = 64;
/// HKDF 用途标签。
const HKDF_INFO: &str = "ees-answer-sheet-v1";

/// 答题卡错误。
#[derive(Debug, thiserror::Error)]
pub enum AnswerSheetError {
    #[error("答题卡文件头不正确（不是本程序生成的文件）")]
    BadMagic,
    #[error("不支持的答题卡版本 {0}")]
    BadVersion(u32),
    #[error("答题卡文件过短")]
    Truncated,
    #[error("试卷指纹格式错误")]
    BadFingerprint,
    #[error("解密失败（密钥不匹配或文件被改动）")]
    Decrypt,
    #[error("答题卡内容损坏：{0}")]
    BadContent(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// 一名学生的答题卡。
///
/// `answers` 与 `qids` 等长同序：`None` 表示未作答，`Some("")` 表示作答但留空。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AnswerSheet {
    pub version: u32,
    pub student_id: u64,
    pub session_id: u32,
    /// 试卷指纹（blake3 hex），绑定"这一份卷"。
    pub paper_fingerprint: String,
    /// 与试卷同序的题目 ID。
    pub qids: Vec<String>,
    /// 每题一个答案字符串；补全/改错是 JSON 数组文本。
    pub answers: Vec<Option<String>>,
    /// 最后修改时间（Unix 秒）。
    pub updated_unix: u64,
    /// 是否已交卷（冻结）。
    pub submitted: bool,
    /// 交卷时间（Unix 秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitted_unix: Option<u64>,
}

impl AnswerSheet {
    /// 依据试卷生成空答题卡。
    ///
    /// **必须**用已抽题后的 `Paper`，而不是题库原始顺序——顺序与数量都以
    /// 试卷为准，否则学生的作答会和题目对不上。
    pub fn from_paper(paper: &Paper, student_id: u64, session_id: u32, now_unix: u64) -> Self {
        let qids: Vec<String> = paper
            .sections
            .iter()
            .flat_map(|s| s.items.iter().map(|i| i.id.clone()))
            .collect();
        let answers = vec![None; qids.len()];
        Self {
            version: VERSION,
            student_id,
            session_id,
            paper_fingerprint: paper.fingerprint(),
            qids,
            answers,
            updated_unix: now_unix,
            submitted: false,
            submitted_unix: None,
        }
    }

    /// 结构自检：长度一致、指纹合法。
    pub fn validate(&self) -> Result<(), AnswerSheetError> {
        if self.version != VERSION {
            return Err(AnswerSheetError::BadVersion(self.version));
        }
        if self.paper_fingerprint.len() != FP_LEN
            || !self
                .paper_fingerprint
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
        {
            return Err(AnswerSheetError::BadFingerprint);
        }
        // 交卷标志与交卷时间必须一致，否则教师端会显示"未交卷"却已冻结
        if self.submitted && self.submitted_unix.is_none() {
            return Err(AnswerSheetError::BadContent(
                "submitted=true 但缺少 submitted_unix".into(),
            ));
        }
        if !self.submitted && self.submitted_unix.is_some() {
            return Err(AnswerSheetError::BadContent(
                "submitted=false 却带有 submitted_unix".into(),
            ));
        }
        if self.qids.len() != self.answers.len() {
            return Err(AnswerSheetError::BadContent(format!(
                "题目数 {} 与答案数 {} 不一致",
                self.qids.len(),
                self.answers.len()
            )));
        }
        Ok(())
    }

    /// 已作答题数。
    pub fn answered(&self) -> usize {
        self.answers.iter().filter(|a| a.is_some()).count()
    }

    /// 冻结（交卷）：之后不再接受修改。
    pub fn freeze(&mut self, now_unix: u64) {
        self.submitted = true;
        self.submitted_unix = Some(now_unix);
        self.updated_unix = now_unix;
    }

    /// 设置第 `idx` 题的答案（已冻结则忽略）。
    pub fn set_answer(&mut self, idx: usize, value: Option<String>, now_unix: u64) -> bool {
        if self.submitted || idx >= self.answers.len() {
            return false;
        }
        if self.answers[idx] == value {
            return false;
        }
        self.answers[idx] = value;
        self.updated_unix = now_unix;
        true
    }

    /// 找到某题目 ID 的下标（校验时用于对齐）。
    pub fn index_of(&self, qid: &str) -> Option<usize> {
        self.qids.iter().position(|q| q == qid)
    }
}

// ---------------------------------------------------------------- 多部分答案编码

/// 把多个空/多条改正项编码成一个字符串（JSON 数组文本）。
///
/// 为什么用 JSON：C 语言答案里 `||`、引号、反斜杠都合法，自定义分隔符
/// 一定要处理转义，而 JSON 是零歧义的。
pub fn encode_parts(parts: &[String]) -> String {
    serde_json::to_string(parts).unwrap_or_else(|_| "[]".to_string())
}

/// 解码 `encode_parts` 的结果；不是 JSON 数组时退化成单元素。
pub fn decode_parts(text: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(text).unwrap_or_else(|_| vec![text.to_string()])
}

/// 改错答案编码：`[{"line":"3","answer":"return x*x;"}]`。
pub fn encode_fixes(fixes: &[(String, String)]) -> String {
    let items: Vec<serde_json::Value> = fixes
        .iter()
        .map(|(line, answer)| serde_json::json!({"line": line, "answer": answer}))
        .collect();
    serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string())
}

/// 解码改错答案。
pub fn decode_fixes(text: &str) -> Vec<(String, String)> {
    serde_json::from_str::<Vec<serde_json::Value>>(text)
        .map(|items| {
            items
                .into_iter()
                .map(|v| {
                    (
                        v.get("line")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        v.get("answer")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------- 加密封装

/// 派生答题卡密钥：`HKDF(K_manifest, salt = 试卷指纹, info = "ees-answer-sheet-v1")`。
pub fn answer_key(
    k_manifest: &[u8; 32],
    paper_fingerprint: &str,
) -> Result<Zeroizing<[u8; 32]>, AnswerSheetError> {
    let salt = hex::decode(paper_fingerprint).map_err(|_| AnswerSheetError::BadFingerprint)?;
    Ok(crypto::hkdf_derive(k_manifest, &salt, HKDF_INFO))
}

fn aad(student_id: u64, session_id: u32, fingerprint: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + 4 + FP_LEN);
    out.extend_from_slice(&student_id.to_le_bytes());
    out.extend_from_slice(&session_id.to_le_bytes());
    out.extend_from_slice(fingerprint.as_bytes());
    out
}

/// 把答题卡封装成落盘字节。
///
/// 布局：`magic(8) | version(4) | student_id(8) | session_id(4) | fp(64) | encrypt(...)`。
/// 头部三项是**明文**——它们不是秘密（文件名里也有），但参与 AAD，
/// 任何一项被改动都会导致解密失败。
pub fn seal(k_manifest: &[u8; 32], sheet: &AnswerSheet) -> Result<Vec<u8>, AnswerSheetError> {
    sheet.validate()?;
    let key = answer_key(k_manifest, &sheet.paper_fingerprint)?;
    let plaintext = serde_json::to_vec(sheet)
        .map_err(|e| AnswerSheetError::BadContent(format!("序列化失败: {e}")))?;
    let ct = crypto::encrypt(
        &key,
        &plaintext,
        &aad(sheet.student_id, sheet.session_id, &sheet.paper_fingerprint),
    )
    .map_err(|_| AnswerSheetError::Decrypt)?;

    let mut out = Vec::with_capacity(8 + 4 + 8 + 4 + FP_LEN + ct.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&sheet.student_id.to_le_bytes());
    out.extend_from_slice(&sheet.session_id.to_le_bytes());
    out.extend_from_slice(sheet.paper_fingerprint.as_bytes());
    out.extend_from_slice(&ct);
    Ok(out)
}

/// 解析答题卡文件的**明文头部**（不解密），用于选择密钥与展示。
#[derive(Debug, Clone, PartialEq)]
pub struct AnswerFileHeader {
    pub version: u32,
    pub student_id: u64,
    pub session_id: u32,
    pub paper_fingerprint: String,
}

/// 读取头部；不校验内容真伪（那是 `open` 的事）。
pub fn read_header(bytes: &[u8]) -> Result<AnswerFileHeader, AnswerSheetError> {
    const HEAD: usize = 8 + 4 + 8 + 4 + FP_LEN;
    if bytes.len() < HEAD {
        return Err(AnswerSheetError::Truncated);
    }
    if &bytes[..8] != MAGIC {
        return Err(AnswerSheetError::BadMagic);
    }
    let version = u32::from_le_bytes(bytes[8..12].try_into().expect("4 字节"));
    if version != VERSION {
        return Err(AnswerSheetError::BadVersion(version));
    }
    let student_id = u64::from_le_bytes(bytes[12..20].try_into().expect("8 字节"));
    let session_id = u32::from_le_bytes(bytes[20..24].try_into().expect("4 字节"));
    let fp = std::str::from_utf8(&bytes[24..HEAD])
        .map_err(|_| AnswerSheetError::BadFingerprint)?
        .to_string();
    Ok(AnswerFileHeader {
        version,
        student_id,
        session_id,
        paper_fingerprint: fp,
    })
}

/// 解密并校验答题卡。
pub fn open(k_manifest: &[u8; 32], bytes: &[u8]) -> Result<AnswerSheet, AnswerSheetError> {
    const HEAD: usize = 8 + 4 + 8 + 4 + FP_LEN;
    let header = read_header(bytes)?;
    if header.paper_fingerprint.len() != FP_LEN {
        return Err(AnswerSheetError::BadFingerprint);
    }
    let key = answer_key(k_manifest, &header.paper_fingerprint)?;
    let plaintext = crypto::decrypt(
        &key,
        &bytes[HEAD..],
        &aad(
            header.student_id,
            header.session_id,
            &header.paper_fingerprint,
        ),
    )
    .map_err(|_| AnswerSheetError::Decrypt)?;
    let sheet: AnswerSheet = serde_json::from_slice(&plaintext)
        .map_err(|e| AnswerSheetError::BadContent(format!("解析 JSON 失败: {e}")))?;
    sheet.validate()?;
    // 头部与内容必须一致（防"换了头部另配一份密文"）
    if sheet.student_id != header.student_id
        || sheet.session_id != header.session_id
        || sheet.paper_fingerprint != header.paper_fingerprint
    {
        return Err(AnswerSheetError::BadContent(
            "文件头与内容不一致（可能被拼接篡改）".into(),
        ));
    }
    Ok(sheet)
}

/// 原子写：先写 `.tmp` 再 rename，断电不会留下半个文件。
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ExamBank, Question};
    use crate::picker::generate_paper;

    fn sheet() -> AnswerSheet {
        let bank = ExamBank {
            exam_id: "m".into(),
            title: "数学".into(),
            pick: 2,
            questions: (0..2)
                .map(|i| Question {
                    id: format!("q{i}"),
                    qtype: "single".into(),
                    stem: "s".into(),
                    options: vec!["a".into(), "b".into()],
                    body: Default::default(),
                    kp: None,
                    difficulty: None,
                    answer: None,
                    judge: None,
                    score: 2,
                })
                .collect(),
        };
        let paper = generate_paper(2021001, 1, b"salt", &[bank], |_e, f| f).unwrap();
        AnswerSheet::from_paper(&paper, 2021001, 1, 1000)
    }

    #[test]
    fn from_paper_matches_item_count() {
        let s = sheet();
        assert_eq!(s.qids.len(), 2);
        assert_eq!(s.answers.len(), 2);
        assert!(s.answers.iter().all(|a| a.is_none()));
        assert!(!s.submitted);
    }

    #[test]
    fn seal_open_roundtrip() {
        let mut s = sheet();
        s.set_answer(0, Some("B".into()), 1001);
        s.set_answer(1, Some(encode_parts(&["a || b".into(), "c".into()])), 1002);
        let k = [7u8; 32];
        let bytes = seal(&k, &s).unwrap();
        let back = open(&k, &bytes).unwrap();
        assert_eq!(back, s);
        assert_eq!(
            decode_parts(&back.answers[1].clone().unwrap()),
            ["a || b", "c"]
        );
    }

    #[test]
    fn wrong_key_fails() {
        let s = sheet();
        let bytes = seal(&[1u8; 32], &s).unwrap();
        assert!(matches!(
            open(&[2u8; 32], &bytes),
            Err(AnswerSheetError::Decrypt)
        ));
    }

    #[test]
    fn tampered_header_fails() {
        let s = sheet();
        let mut bytes = seal(&[3u8; 32], &s).unwrap();
        // 改学号（AAD 的一部分）
        bytes[12] ^= 0xFF;
        assert!(matches!(
            open(&[3u8; 32], &bytes),
            Err(AnswerSheetError::Decrypt)
        ));
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let s = sheet();
        let mut bytes = seal(&[4u8; 32], &s).unwrap();
        let n = bytes.len();
        bytes[n - 1] ^= 0x01;
        assert!(matches!(
            open(&[4u8; 32], &bytes),
            Err(AnswerSheetError::Decrypt)
        ));
    }

    #[test]
    fn bad_magic_rejected() {
        let mut bytes = seal(&[5u8; 32], &sheet()).unwrap();
        bytes[0] = b'X';
        assert!(matches!(
            read_header(&bytes),
            Err(AnswerSheetError::BadMagic)
        ));
    }

    #[test]
    fn frozen_sheet_rejects_edits() {
        let mut s = sheet();
        s.freeze(2000);
        assert!(!s.set_answer(0, Some("A".into()), 2001));
        assert_eq!(s.answers[0], None);
        assert!(s.submitted);
        assert_eq!(s.submitted_unix, Some(2000));
    }

    #[test]
    fn length_mismatch_rejected() {
        let mut s = sheet();
        s.answers.pop();
        assert!(matches!(s.validate(), Err(AnswerSheetError::BadContent(_))));
        assert!(seal(&[6u8; 32], &s).is_err());
    }

    #[test]
    fn parts_and_fixes_roundtrip() {
        let parts = vec!["a || b".to_string(), "\"引号\"".to_string(), "".to_string()];
        assert_eq!(decode_parts(&encode_parts(&parts)), parts);

        let fixes = vec![
            ("3".to_string(), "return x * x;".to_string()),
            ("7".to_string(), "if(i % 2 == 0)".to_string()),
        ];
        assert_eq!(decode_fixes(&encode_fixes(&fixes)), fixes);
    }

    #[test]
    fn write_atomic_replaces_file() {
        let dir = std::path::Path::new("target").join("answers-test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.eea");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        assert!(!p.with_extension("tmp").exists(), "不应残留 .tmp");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn submitted_must_have_timestamp() {
        let mut s = sheet();
        s.submitted = true; // 没有 submitted_unix
        assert!(matches!(s.validate(), Err(AnswerSheetError::BadContent(_))));
        s.submitted_unix = Some(5);
        assert!(s.validate().is_ok());

        let mut t = sheet();
        t.submitted_unix = Some(5); // 没交卷却有时间
        assert!(matches!(t.validate(), Err(AnswerSheetError::BadContent(_))));
    }

    #[test]
    fn answered_counts_only_some() {
        let mut s = sheet();
        s.set_answer(0, Some("A".into()), 1);
        assert_eq!(s.answered(), 1);
        s.set_answer(1, Some(String::new()), 2); // 作答但留空也算"已作答"
        assert_eq!(s.answered(), 2);
    }
}
