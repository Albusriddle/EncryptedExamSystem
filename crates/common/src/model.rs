//! 数据模型：题库、场次、清单（manifest）。
//!
//! `schemas/*.schema.json` 由管理端的 pydantic 模型生成
//! （`cd admin && uv run python buildtools/gen_schemas.py`），
//! `admin/tests/test_schemas.py` 会在两者不一致时失败。

use serde::{Deserialize, Serialize};

/// 代码简答题支持的语言（与客户端 syntect、管理端校验、文档保持一致）。
pub const CODE_LANGUAGES: [&str; 11] = [
    "python",
    "c",
    "cpp",
    "java",
    "rust",
    "javascript",
    "typescript",
    "go",
    "sql",
    "bash",
    "text",
];
/// 单题允许语言数量上限。
pub const MAX_CODE_LANGUAGES: usize = 8;

/// 单题最多几个输入/输出示例。
pub const MAX_SAMPLES: usize = 20;
/// 单个示例（输入 + 输出）的字节上限。
pub const MAX_SAMPLE_BYTES: usize = 4096;

/// 一道题的输入/输出示例。
///
/// 只用于展示（学生端渲染成等宽的输入框/输出框），不参与判分。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QuestionSample {
    /// 输入内容（可为空：程序不需要输入时）。
    #[serde(default)]
    pub input: String,
    /// 期望输出（保留换行与行尾空格）。
    #[serde(default)]
    pub output: String,
}

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
    /// 题型：single / multiple / judge / fill / fix / essay / code。
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
    /// 代码简答题**单个**允许语言（旧字段，兼容既有题库）。
    ///
    /// 与 `code_languages` **不共存**：两个都有时以 `code_languages` 为准。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_language: Option<String>,
    /// 代码简答题**多个**允许语言（新字段）。
    ///
    /// 非空时忽略 `code_language`。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub code_languages: Vec<String>,
    /// 输入/输出示例（学生端渲染成独立的输入框/输出框）。
    ///
    /// 只影响显示；旧学生端会忽略该字段。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub samples: Vec<QuestionSample>,
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

impl Question {
    /// 把导入/手工清单里的旧题型别名迁移为标准值。
    ///
    /// 别名只在入库时兼容；校验和渲染仍只认标准值。
    /// 归一语言字段：把 `code_languages` 去重小写；若只有单个 `code_language`，
    /// 保持原样（规范要求单语言只写 `code_language`）。
    pub fn normalize_code_languages(&mut self) {
        let mut out: Vec<String> = Vec::new();
        for lang in &self.code_languages {
            let l = lang.trim().to_ascii_lowercase();
            if !l.is_empty() && !out.contains(&l) {
                out.push(l);
            }
        }
        self.code_languages = out;
        if let Some(single) = self.code_language.take() {
            let l = single.trim().to_ascii_lowercase();
            self.code_language = if l.is_empty() { None } else { Some(l) };
        }
    }

    pub fn normalize_qtype(&mut self) {
        self.qtype = match self.qtype.as_str() {
            "multi" => "multiple".into(),
            "blank" | "blanks" => "fill".into(),
            "fix_code" => "fix".into(),
            other => other.into(),
        };
    }
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
    /// 迁移旧题型别名；builder 在 validate 前调用。
    pub fn normalize_qtypes(&mut self) {
        for q in &mut self.questions {
            q.normalize_qtype();
            q.normalize_code_languages();
        }
    }

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
            let allowed = [
                "single", "multiple", "judge", "fill", "fix", "essay", "code",
            ];
            if !allowed.contains(&q.qtype.as_str()) {
                return Err(crate::model::ModelError::UnknownQuestionType {
                    exam_id: self.exam_id.clone(),
                    qid: q.id.clone(),
                    qtype: q.qtype.clone(),
                });
            }
            if q.qtype == "code" && !matches!(q.body, QuestionBody::Text) {
                return Err(crate::model::ModelError::CodeQuestionRequiresTextBody {
                    exam_id: self.exam_id.clone(),
                    qid: q.id.clone(),
                });
            }
            if q.qtype == "code" {
                if let Some(lang) = q.code_language.as_deref()
                    && !CODE_LANGUAGES.contains(&lang)
                {
                    return Err(crate::model::ModelError::InvalidCodeLanguage {
                        exam_id: self.exam_id.clone(),
                        qid: q.id.clone(),
                        language: lang.to_string(),
                    });
                }
                if q.code_languages.len() > MAX_CODE_LANGUAGES {
                    return Err(crate::model::ModelError::TooManyCodeLanguages {
                        exam_id: self.exam_id.clone(),
                        qid: q.id.clone(),
                        count: q.code_languages.len(),
                    });
                }
                for lang in &q.code_languages {
                    if !CODE_LANGUAGES.contains(&lang.as_str()) {
                        return Err(crate::model::ModelError::InvalidCodeLanguage {
                            exam_id: self.exam_id.clone(),
                            qid: q.id.clone(),
                            language: lang.clone(),
                        });
                    }
                }
                // 不共存：两个字段同时填写属于出题错误，直接拒绝，
                // 避免"以为写了多语言、实际被单语言覆盖"。
                if !q.code_languages.is_empty() && q.code_language.is_some() {
                    return Err(crate::model::ModelError::CodeLanguageConflict {
                        exam_id: self.exam_id.clone(),
                        qid: q.id.clone(),
                    });
                }
            }
            if q.samples.len() > MAX_SAMPLES {
                return Err(crate::model::ModelError::TooManySamples {
                    exam_id: self.exam_id.clone(),
                    qid: q.id.clone(),
                    count: q.samples.len(),
                });
            }
            for (idx, sample) in q.samples.iter().enumerate() {
                let bytes = sample.input.len() + sample.output.len();
                if bytes > MAX_SAMPLE_BYTES {
                    return Err(crate::model::ModelError::SampleTooLarge {
                        exam_id: self.exam_id.clone(),
                        qid: q.id.clone(),
                        index: idx + 1,
                        bytes,
                    });
                }
                if sample.input.trim().is_empty() && sample.output.trim().is_empty() {
                    return Err(crate::model::ModelError::EmptySample {
                        exam_id: self.exam_id.clone(),
                        qid: q.id.clone(),
                        index: idx + 1,
                    });
                }
            }
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

/// 大题下的一个部分：固定题数、精确难度。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SectionPart {
    pub part_id: String,
    pub title: String,
    pub count: u32,
    pub difficulty: u8,
}

/// 场次里的一个大题：固定题库；可再分若干部分。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Section {
    pub section_id: String,
    pub title: String,
    pub file: String,
    /// 旧格式汇总题数；新格式由 parts 汇总（可省略）。
    #[serde(default)]
    pub count: u32,
    /// 旧格式难度；新格式取第一个部分的难度（可省略）。
    #[serde(default)]
    pub difficulty: u8,
    #[serde(default)]
    pub parts: Vec<SectionPart>,
}

impl Section {
    /// 统一取部分列表：旧单层配置视为“只有一个部分”。
    pub fn effective_parts(&self) -> Vec<SectionPart> {
        if self.parts.is_empty() {
            vec![SectionPart {
                part_id: format!("{}p1", self.section_id),
                title: format!("{} · 全部", self.title),
                count: self.count,
                difficulty: self.difficulty,
            }]
        } else {
            self.parts.clone()
        }
    }
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
    /// 本场包含的题库与抽题数（旧“按平均难度”模式）。
    pub exams: Vec<SessionExam>,
    /// 本场的大题列表（新“按大题”模式）；为空时用旧模式。
    #[serde(default)]
    pub sections: Vec<Section>,
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

/// 学生交卷后是否允许免授权回看（教师端 review-paper 始终可用）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum StudentReviewPolicy {
    /// 学生端不显示回看；考后由教师端用 master 查看原卷。
    #[default]
    None,
    /// 已交卷且未到结束时间时，学生可免授权只读回看。
    Free,
}

impl StudentReviewPolicy {
    /// 学生端回看是否可用：必须同时已交卷且未到结束时间。
    /// 教师端 `review-paper` 不受此函数限制。
    pub fn allows_review(self, submitted: bool, expired: bool) -> bool {
        matches!(self, Self::Free) && submitted && !expired
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
    #[serde(default)]
    pub student_review: StudentReviewPolicy,
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
    #[error(
        "题库 {exam_id} 的题 {qid} 使用了未知题型：{qtype}；支持 single/multiple/judge/fill/fix/essay/code；旧别名 multi→multiple、blank→fill、fix_code→fix"
    )]
    UnknownQuestionType {
        exam_id: String,
        qid: String,
        qtype: String,
    },
    #[error("题库 {exam_id} 的代码题 {qid} 必须使用 body.kind=text")]
    CodeQuestionRequiresTextBody { exam_id: String, qid: String },
    #[error("题库 {exam_id} 的代码题 {qid} 允许语言过多：{count}（上限 {MAX_CODE_LANGUAGES}）")]
    TooManyCodeLanguages {
        exam_id: String,
        qid: String,
        count: usize,
    },
    #[error("题库 {exam_id} 的代码题 {qid} 同时写了 code_language 与 code_languages；二者不共存")]
    CodeLanguageConflict { exam_id: String, qid: String },
    #[error("题库 {exam_id} 的题 {qid} 示例过多：{count}（上限 {MAX_SAMPLES}）")]
    TooManySamples {
        exam_id: String,
        qid: String,
        count: usize,
    },
    #[error(
        "题库 {exam_id} 的题 {qid} 第 {index} 个示例过大：{bytes} 字节（上限 {MAX_SAMPLE_BYTES}）"
    )]
    SampleTooLarge {
        exam_id: String,
        qid: String,
        index: usize,
        bytes: usize,
    },
    #[error("题库 {exam_id} 的题 {qid} 第 {index} 个示例的输入和输出都为空")]
    EmptySample {
        exam_id: String,
        qid: String,
        index: usize,
    },
    #[error("题库 {exam_id} 的代码题 {qid} 使用了不支持的语言：{language}")]
    InvalidCodeLanguage {
        exam_id: String,
        qid: String,
        language: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_question_requires_text_body() {
        let bank = ExamBank {
            exam_id: "m".into(),
            title: "m".into(),
            pick: 1,
            questions: vec![Question {
                id: "c1".into(),
                qtype: "code".into(),
                stem: "s".into(),
                options: vec![],
                code_language: None,
                code_languages: Vec::new(),
                samples: Vec::new(),
                body: QuestionBody::Code {
                    code: vec!["x".into()],
                    blank_count: 1,
                },
                kp: None,
                difficulty: None,
                answer: None,
                judge: None,
                score: 2,
            }],
        };
        assert!(matches!(
            bank.validate(),
            Err(ModelError::CodeQuestionRequiresTextBody { .. })
        ));
    }

    fn code_q(languages: Vec<&str>, single: Option<&str>) -> ExamBank {
        ExamBank {
            exam_id: "m".into(),
            title: "m".into(),
            pick: 1,
            questions: vec![Question {
                id: "c1".into(),
                qtype: "code".into(),
                stem: "s".into(),
                options: vec![],
                body: QuestionBody::Text,
                code_language: single.map(str::to_string),
                code_languages: languages.into_iter().map(str::to_string).collect(),
                samples: Vec::new(),
                kp: None,
                difficulty: None,
                answer: None,
                judge: None,
                score: 2,
            }],
        }
    }

    #[test]
    fn samples_are_validated() {
        let mut bank = code_q(vec!["c"], None);
        bank.questions[0].samples = vec![QuestionSample {
            input: "3".into(),
            output: "*\n**\n***".into(),
        }];
        assert!(bank.validate().is_ok(), "正常示例应通过");

        bank.questions[0].samples = vec![QuestionSample::default()];
        assert!(matches!(
            bank.validate(),
            Err(ModelError::EmptySample { .. })
        ));

        bank.questions[0].samples = vec![QuestionSample {
            input: "x".repeat(MAX_SAMPLE_BYTES),
            output: "y".into(),
        }];
        assert!(matches!(
            bank.validate(),
            Err(ModelError::SampleTooLarge { .. })
        ));

        bank.questions[0].samples = (0..MAX_SAMPLES + 1)
            .map(|i| QuestionSample {
                input: i.to_string(),
                output: i.to_string(),
            })
            .collect();
        assert!(matches!(
            bank.validate(),
            Err(ModelError::TooManySamples { .. })
        ));
    }

    #[test]
    fn code_languages_accepts_multiple() {
        let bank = code_q(vec!["c", "cpp", "python"], None);
        assert!(bank.validate().is_ok());
    }

    #[test]
    fn code_language_and_languages_conflict() {
        let bank = code_q(vec!["c", "python"], Some("c"));
        assert!(matches!(
            bank.validate(),
            Err(ModelError::CodeLanguageConflict { .. })
        ));
    }

    #[test]
    fn too_many_code_languages_rejected() {
        let bank = code_q(
            vec![
                "c", "cpp", "python", "java", "rust", "go", "sql", "bash", "text",
            ],
            None,
        );
        assert!(matches!(
            bank.validate(),
            Err(ModelError::TooManyCodeLanguages { .. })
        ));
    }

    #[test]
    fn invalid_code_language_is_rejected() {
        let bank = ExamBank {
            exam_id: "m".into(),
            title: "m".into(),
            pick: 1,
            questions: vec![Question {
                id: "c1".into(),
                qtype: "code".into(),
                stem: "s".into(),
                options: vec![],
                body: QuestionBody::Text,
                code_language: Some("brainfuck".into()),
                code_languages: Vec::new(),
                samples: Vec::new(),
                kp: None,
                difficulty: None,
                answer: None,
                judge: None,
                score: 2,
            }],
        };
        assert!(matches!(
            bank.validate(),
            Err(ModelError::InvalidCodeLanguage { .. })
        ));
    }

    #[test]
    fn normalize_legacy_qtypes() {
        let mut bank = ExamBank {
            exam_id: "m".into(),
            title: "m".into(),
            pick: 1,
            questions: vec![Question {
                id: "q".into(),
                qtype: "multi".into(),
                stem: "s".into(),
                options: vec![],
                code_language: None,
                code_languages: Vec::new(),
                samples: Vec::new(),
                body: QuestionBody::Text,
                kp: None,
                difficulty: None,
                answer: None,
                judge: None,
                score: 2,
            }],
        };
        bank.normalize_qtypes();
        assert_eq!(bank.questions[0].qtype, "multiple");
        assert!(bank.validate().is_ok());
    }

    #[test]
    fn unknown_question_type_is_rejected() {
        let bank = ExamBank {
            exam_id: "m".into(),
            title: "m".into(),
            pick: 1,
            questions: vec![Question {
                id: "q".into(),
                qtype: "Code".into(),
                stem: "s".into(),
                options: vec![],
                code_language: None,
                code_languages: Vec::new(),
                samples: Vec::new(),
                body: QuestionBody::Text,
                kp: None,
                difficulty: None,
                answer: None,
                judge: None,
                score: 2,
            }],
        };
        assert!(matches!(
            bank.validate(),
            Err(ModelError::UnknownQuestionType { .. })
        ));
    }

    #[test]
    fn review_policy_requires_free_submitted_and_not_expired() {
        assert!(!StudentReviewPolicy::None.allows_review(true, false));
        assert!(!StudentReviewPolicy::Free.allows_review(false, false));
        assert!(StudentReviewPolicy::Free.allows_review(true, false));
        assert!(!StudentReviewPolicy::Free.allows_review(true, true));
    }

    fn q_with_options(n: usize) -> Question {
        Question {
            id: "q".into(),
            qtype: "multiple".into(),
            stem: "s".into(),
            options: Vec::new(),
            code_language: None,
            code_languages: Vec::new(),
            samples: Vec::new(),
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
