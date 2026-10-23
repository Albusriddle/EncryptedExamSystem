//! 数据模型：题库、场次、清单（manifest）。
//!
//! `schemas/*.schema.json` 由管理端的 pydantic 模型生成
//! （`cd admin && uv run python buildtools/gen_schemas.py`），
//! `admin/tests/test_schemas.py` 会在两者不一致时失败。

use serde::{Deserialize, Serialize};

/// 单道题目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Question {
    /// 题库内唯一 ID。
    pub id: String,
    /// 题型：single / multiple / judge / fill / essay。
    #[serde(rename = "type")]
    pub qtype: String,
    /// 题干。
    pub stem: String,
    /// 选项（选择/判断题使用）。
    #[serde(default)]
    pub options: Vec<String>,
    /// 标准答案（打包进学生端时会被剥离）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// 分值。
    #[serde(default = "default_score")]
    pub score: u32,
}

fn default_score() -> u32 {
    2
}

/// 一个题库文件（对应一门考试 / 一个 json）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExamBank {
    /// 题库标识，例如 "math-2026-final"。
    pub exam_id: String,
    pub title: String,
    /// 本场考试从该题库抽取的题数（由管理端设置）。
    pub pick: u32,
    pub questions: Vec<Question>,
}

impl ExamBank {
    /// 校验题库基本合法性。
    pub fn validate(&self) -> Result<(), crate::model::ModelError> {
        if self.exam_id.is_empty() {
            return Err(crate::model::ModelError::EmptyExamId);
        }
        if self.pick as usize > self.questions.len() {
            return Err(crate::model::ModelError::PickExceedsPool {
                exam_id: self.exam_id.clone(),
                pick: self.pick,
                pool: self.questions.len(),
            });
        }
        let mut seen = std::collections::HashSet::new();
        for q in &self.questions {
            if !seen.insert(&q.id) {
                return Err(crate::model::ModelError::DuplicateQuestionId {
                    exam_id: self.exam_id.clone(),
                    qid: q.id.clone(),
                });
            }
        }
        Ok(())
    }
}

/// 场次中的一门考试引用。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionExam {
    /// 指向 banks/ 下的文件名，例如 "math.json"。
    pub file: String,
    /// 覆盖题库自带的 pick；为空则沿用题库内 pick。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pick: Option<u32>,
}

/// 一个考试场次。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Session {
    /// 场次编号。
    pub session_id: u32,
    /// 开始时间（RFC3339，建议带时区偏移）。
    pub start: String,
    /// 结束时间。
    pub end: String,
    /// 该场考生学号列表。
    pub students: Vec<u64>,
    /// 本场包含的题库与抽题数。
    pub exams: Vec<SessionExam>,
}

/// 顶层构建清单（管理端产出，builder 消费）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub version: u32,
    /// 全局 salt（hex），参与密钥派生与抽题种子。
    pub salt: String,
    pub sessions: Vec<Session>,
    // 说明：这里曾有 `time_sources` / `time_pubkey` 两个字段，
    // 但**从未有任何代码读取它们**——时间源在客户端是硬编码的三个
    // 公共站点。留着会让人以为"可以自定义时间源"或"时间有签名校验"，
    // 实际上都没有。已删除，避免配置了却不生效的误导。
    /// Argon2id 参数（随包存储，便于将来调参而不作废旧包）。
    #[serde(default)]
    pub kdf: crate::crypto::KdfParams,
}

/// 模型校验错误。
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("exam_id 为空")]
    EmptyExamId,
    #[error("题库 {exam_id} 抽题数 {pick} 超过题量 {pool}")]
    PickExceedsPool {
        exam_id: String,
        pick: u32,
        pool: usize,
    },
    #[error("题库 {exam_id} 存在重复题目 ID: {qid}")]
    DuplicateQuestionId { exam_id: String, qid: String },
}
