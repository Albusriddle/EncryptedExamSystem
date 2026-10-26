//! 数据模型：题库、场次、清单（manifest）。
//!
//! `schemas/*.schema.json` 由管理端的 pydantic 模型生成
//! （`cd admin && uv run python buildtools/gen_schemas.py`），
//! `admin/tests/test_schemas.py` 会在两者不一致时失败。

use serde::{Deserialize, Serialize};

/// 一个选项。保留键（A/B/C…），顺序即呈现顺序。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChoiceOption {
    pub key: String,
    pub text: String,
}

/// 题目正文的形态。
///
/// 为什么不用"一个 `options` 数组打天下"：补全题是**带占位符的代码模板**，
/// 改错题是**逐行代码 + 若干待改行**，它们既不是选项、也不该被压成一段
/// 纯文本（压了就没法渲染代码块、没法定位第几行）。
///
/// `Text` 是默认值：老题库没有 `body` 字段，反序列化后落到这里，
/// 渲染时回退到 `Question::options`，因此**旧题库一行都不用改**。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QuestionBody {
    /// 纯题干（无选项、无代码块）
    #[default]
    Text,
    /// 选择题：带键的选项
    Options { options: Vec<ChoiceOption> },
    /// 补全题：代码模板，内含 `____①____` 形式的占位符
    Template {
        template: String,
        #[serde(default)]
        blank_count: u8,
    },
    /// 改错题：逐行代码（行号从 1 开始，含空行）
    Code {
        code: Vec<String>,
        #[serde(default)]
        blank_count: u8,
    },
}

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
    /// 题目正文形态（旧题库缺省为 `Text`，此时用 `options` 渲染）。
    #[serde(default)]
    pub body: QuestionBody,
    /// 知识点编号（如 "1.1"，外部题库才有）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kp: Option<String>,
    /// 难度；外部题库约定 `0` 表示多选题。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub difficulty: Option<u8>,
    /// 标准答案（结构化，打包进学生端时被剥离）。
    ///
    /// 用 `Value` 而不是具体枚举：答案只在管理端（Python）需要理解，
    /// 学生端与 builder 都不读它。在 Rust 里再定义一套答案枚举只会
    /// 产生"两端各写一遍、迟早不一致"的重复。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<serde_json::Value>,
    /// 判分材料（`{entry, harness, tests}`，外部题库才有）。
    ///
    /// **绝不进包**：里面可能含完整 `main()` 与预期输出。打包前由管理端剥离，
    /// 这里保留是为了将来做判分时不丢信息。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge: Option<serde_json::Value>,
    /// 分值。
    #[serde(default = "default_score")]
    pub score: u32,
}

fn default_score() -> u32 {
    2
}

/// 选项序号 → 显示标签（0→A，25→Z，26→"27"）。
///
/// 放在 common 里是**唯一实现**：管理端导出、builder 校验、学生端渲染
/// 都用它，避免"两处各写一份、边界处不一致"（旧代码在 client 里有一份）。
pub fn option_label(j: usize) -> String {
    if j < 26 {
        ((b'A' + j as u8) as char).to_string()
    } else {
        format!("{}", j + 1)
    }
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
            // 选项键只能是 A–Z：答题卡用"字母拼接"表示多选答案，
            // 超过 26 个会出现 "27" 这种多字符键，和 "2"+"7" 无法区分。
            let option_count = match &q.body {
                crate::model::QuestionBody::Options { options } => options.len(),
                _ => q.options.len(), // 旧格式只有 options 数组
            };
            if option_count > 26 {
                return Err(crate::model::ModelError::TooManyOptions {
                    exam_id: self.exam_id.clone(),
                    qid: q.id.clone(),
                    count: option_count,
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
    /// 平均难度区间（1..255）。两者要么都为 None（不限制），要么都设置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub difficulty_min: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub difficulty_max: Option<u8>,
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

/// 签名时间令牌配置（阶段 4.2）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimeConfig {
    /// off / prefer / require
    #[serde(default = "default_time_mode")]
    pub mode: String,
    #[serde(default)]
    pub url: String,
    /// base64(Ed25519 32B)
    #[serde(default)]
    pub pubkey: String,
    #[serde(default = "default_time_skew")]
    pub max_skew: u64,
}

fn default_time_mode() -> String {
    "off".to_string()
}
fn default_time_skew() -> u64 {
    300
}

impl Default for TimeConfig {
    fn default() -> Self {
        Self {
            mode: default_time_mode(),
            url: String::new(),
            pubkey: String::new(),
            max_skew: default_time_skew(),
        }
    }
}

impl TimeConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.mode == "off" {
            return Ok(());
        }
        if self.mode != "prefer" && self.mode != "require" {
            return Err(format!(
                "时间令牌 mode 必须是 off/prefer/require，实际 {}",
                self.mode
            ));
        }
        if !self.url.starts_with("https://") {
            return Err("时间令牌 URL 必须以 https:// 开头".into());
        }
        use base64::Engine as _;
        let raw = base64::engine::general_purpose::STANDARD
            .decode(&self.pubkey)
            .map_err(|_| "时间令牌公钥不是合法 base64".to_string())?;
        if raw.len() != 32 {
            return Err("时间令牌公钥必须是 32 字节 Ed25519 公钥".into());
        }
        Ok(())
    }
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
    #[serde(default)]
    pub time: TimeConfig,
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
    #[error("题库 {exam_id} 的题 {qid} 有 {count} 个选项，超过 26 个（A–Z）")]
    TooManyOptions {
        exam_id: String,
        qid: String,
        count: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q_with_options(n: usize) -> Question {
        Question {
            id: "q".into(),
            qtype: "multiple".into(),
            stem: "s".into(),
            options: Vec::new(),
            body: QuestionBody::Options {
                options: (0..n)
                    .map(|i| ChoiceOption {
                        key: crate::model::option_label(i),
                        text: "x".into(),
                    })
                    .collect(),
            },
            kp: None,
            difficulty: None,
            answer: None,
            judge: None,
            score: 2,
        }
    }

    #[test]
    fn rejects_more_than_26_options() {
        let bank = ExamBank {
            exam_id: "m".into(),
            title: "t".into(),
            pick: 1,
            questions: vec![q_with_options(27)],
        };
        assert!(matches!(
            bank.validate(),
            Err(ModelError::TooManyOptions { .. })
        ));
    }

    #[test]
    fn allows_26_options() {
        let bank = ExamBank {
            exam_id: "m".into(),
            title: "t".into(),
            pick: 1,
            questions: vec![q_with_options(26)],
        };
        assert!(bank.validate().is_ok());
    }
}
