//! 确定性抽题：固定 (学号, 场次, salt) → 固定试卷，跨平台/跨版本稳定。
//!
//! 稳定性三原则：
//!
//! 1. 种子用 blake3 显式算出 32 字节，不依赖 rand 内部的 seed_from_u64 细节；
//! 2. 随机源用 ChaCha8Rng（算法固定，rand_chacha 承诺跨版本一致）；
//! 3. 洗牌自己实现 Fisher-Yates，只依赖 next_u32()，不用 slice::shuffle
//!    （后者实现可能随 rand 大版本变化）。

use rand::RngCore;
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::SeedableRng;

use crate::model::ExamBank;

/// 抽题结果：每门考试选中的题目 ID 列表。
#[derive(Debug, Clone, PartialEq)]
pub struct Paper {
    pub student_id: u64,
    pub session_id: u32,
    /// (exam_id, 选中的题目 id 列表)
    pub sections: Vec<(String, Vec<String>)>,
}

impl Paper {
    /// 试卷内容哈希，用于"同一学号同场次多次运行结果一致"的校验与泄题溯源。
    ///
    /// **每个字段都带长度前缀**：否则 `["ab","c"]` 与 `["a","bc"]` 拼接后
    /// 字节流相同，会产生指纹碰撞（实测可复现），水印溯源就失去意义。
    pub fn fingerprint(&self) -> String {
        let mut h = blake3::Hasher::new();
        h.update(b"exam-paper-fp-v2");
        h.update(&self.student_id.to_le_bytes());
        h.update(&self.session_id.to_le_bytes());
        h.update(&(self.sections.len() as u32).to_le_bytes());
        for (exam_id, ids) in &self.sections {
            write_len_prefixed(&mut h, exam_id.as_bytes());
            h.update(&(ids.len() as u32).to_le_bytes());
            for id in ids {
                write_len_prefixed(&mut h, id.as_bytes());
            }
        }
        h.finalize().to_hex().to_string()
    }
}

/// 写入 `长度(4 字节 LE) || 内容`，消除拼接歧义。
fn write_len_prefixed(h: &mut blake3::Hasher, bytes: &[u8]) {
    h.update(&(bytes.len() as u32).to_le_bytes());
    h.update(bytes);
}

/// 由学号、场次、salt 构造确定性 RNG。
fn make_rng(student_id: u64, session_id: u32, salt: &[u8]) -> ChaCha8Rng {
    let mut h = blake3::Hasher::new();
    h.update(b"exam-paper-seed-v1");
    h.update(&student_id.to_le_bytes());
    h.update(&session_id.to_le_bytes());
    h.update(salt);
    let seed: [u8; 32] = h.finalize().into();
    ChaCha8Rng::from_seed(seed)
}

/// 手写 Fisher-Yates，只用 next_u32()，保证跨版本一致。
fn fisher_yates_shuffle(items: &mut [usize], rng: &mut ChaCha8Rng) {
    let n = items.len();
    if n <= 1 {
        return;
    }
    for i in (1..n).rev() {
        // 无偏采样：拒绝法消除取模偏差。
        let bound = (i + 1) as u32;
        let limit = u32::MAX - (u32::MAX % bound);
        let j = loop {
            let x = rng.next_u32();
            if x < limit {
                break (x % bound) as usize;
            }
        };
        items.swap(i, j);
    }
}

/// 抽题错误。
#[derive(Debug, thiserror::Error)]
pub enum PickError {
    #[error("题库 {exam_id} 要求抽 {pick} 题，但只有 {pool} 题")]
    PickExceedsPool {
        exam_id: String,
        pick: u32,
        pool: usize,
    },
}

/// 从题库中确定性抽取 `pick` 道题，返回题目 id。
///
/// `pick` 由调用方给出（来自 payload 的 picks 表），**不读** `bank.pick`，
/// 避免"清单设置的抽题数被题库默认值覆盖"。
fn pick_from_bank(
    bank: &ExamBank,
    pick: u32,
    rng: &mut ChaCha8Rng,
) -> Result<Vec<String>, PickError> {
    let pool_len = bank.questions.len();
    let n = pick as usize;
    if n > pool_len {
        return Err(PickError::PickExceedsPool {
            exam_id: bank.exam_id.clone(),
            pick,
            pool: pool_len,
        });
    }
    let mut pool: Vec<usize> = (0..pool_len).collect();
    fisher_yates_shuffle(&mut pool, rng);
    Ok(pool[..n]
        .iter()
        .map(|&i| bank.questions[i].id.clone())
        .collect())
}

/// 生成一份完整试卷。
///
/// `salt` 取 manifest.salt 的原始字节（hex 解码后）。
/// `pick_for` 返回每个题库的实际抽题数；返回 `None` 时回退到题库自带 pick。
pub fn generate_paper<F>(
    student_id: u64,
    session_id: u32,
    salt: &[u8],
    banks: &[ExamBank],
    pick_for: F,
) -> Result<Paper, PickError>
where
    F: Fn(&str, u32) -> u32,
{
    let mut rng = make_rng(student_id, session_id, salt);
    let mut sections = Vec::with_capacity(banks.len());
    for b in banks {
        let pick = pick_for(&b.exam_id, b.pick);
        let ids = pick_from_bank(b, pick, &mut rng)?;
        sections.push((b.exam_id.clone(), ids));
    }
    Ok(Paper {
        student_id,
        session_id,
        sections,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo_bank(exam_id: &str, n: usize, pick: u32) -> ExamBank {
        ExamBank {
            exam_id: exam_id.to_string(),
            title: exam_id.to_string(),
            pick,
            questions: (0..n)
                .map(|i| crate::model::Question {
                    id: format!("q{i:04}"),
                    qtype: "single".into(),
                    stem: format!("题目 {i}"),
                    options: vec!["A".into(), "B".into(), "C".into(), "D".into()],
                    answer: Some("A".into()),
                    score: 2,
                })
                .collect(),
        }
    }

    /// 用题库自带 pick 抽题的便捷封装（测试用）。
    fn paper_default(student: u64, session: u32, salt: &[u8], banks: &[ExamBank]) -> Paper {
        generate_paper(student, session, salt, banks, |_, fallback| fallback).expect("抽题不应失败")
    }

    #[test]
    fn same_seed_same_paper() {
        let banks = vec![demo_bank("math", 100, 20), demo_bank("english", 80, 15)];
        let p1 = paper_default(2021001, 1, b"salt", &banks);
        let p2 = paper_default(2021001, 1, b"salt", &banks);
        assert_eq!(p1, p2);
        assert_eq!(p1.fingerprint(), p2.fingerprint());
    }

    #[test]
    fn different_student_different_paper() {
        let banks = vec![demo_bank("math", 100, 20)];
        let p1 = paper_default(2021001, 1, b"salt", &banks);
        let p2 = paper_default(2021002, 1, b"salt", &banks);
        assert_ne!(p1.sections, p2.sections);
    }

    #[test]
    fn different_session_different_paper() {
        let banks = vec![demo_bank("math", 100, 20)];
        let p1 = paper_default(2021001, 1, b"salt", &banks);
        let p2 = paper_default(2021001, 2, b"salt", &banks);
        assert_ne!(p1.sections, p2.sections);
    }

    #[test]
    fn pick_count_and_uniqueness() {
        let banks = vec![demo_bank("math", 100, 20)];
        let p = paper_default(2021001, 1, b"salt", &banks);
        let ids = &p.sections[0].1;
        assert_eq!(ids.len(), 20);
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 20, "抽题结果不应有重复");
    }

    /// 回归：清单里设置的抽题数必须真正生效（曾经被题库自带 pick 覆盖）。
    #[test]
    fn explicit_pick_overrides_bank_default() {
        let banks = vec![demo_bank("math", 100, 3)];
        let p = generate_paper(2021001, 1, b"salt", &banks, |_, _| 25).unwrap();
        assert_eq!(p.sections[0].1.len(), 25, "应遵循显式 pick 而非题库默认值");
    }

    /// 回归：pick 超过题量必须返回错误，而不是 panic（曾经会越界 panic）。
    #[test]
    fn pick_exceeds_pool_returns_error() {
        let banks = vec![demo_bank("math", 6, 10)];
        let err = generate_paper(2021001, 1, b"salt", &banks, |_, _| 10).unwrap_err();
        assert!(matches!(
            err,
            PickError::PickExceedsPool {
                pick: 10,
                pool: 6,
                ..
            }
        ));
    }

    /// **回归**：拼接歧义曾导致指纹碰撞（实测可复现）。
    #[test]
    fn fingerprint_has_no_concatenation_collision() {
        let a = Paper {
            student_id: 1,
            session_id: 1,
            sections: vec![("ab".into(), vec!["c".into()])],
        };
        let b = Paper {
            student_id: 1,
            session_id: 1,
            sections: vec![("a".into(), vec!["bc".into()])],
        };
        assert_ne!(a.fingerprint(), b.fingerprint(), "拼接歧义必须被消除");

        // 题目 id 列表的分组也要有边界
        let c = Paper {
            student_id: 1,
            session_id: 1,
            sections: vec![("x".into(), vec!["ab".into(), "c".into()])],
        };
        let d = Paper {
            student_id: 1,
            session_id: 1,
            sections: vec![("x".into(), vec!["a".into(), "bc".into()])],
        };
        assert_ne!(c.fingerprint(), d.fingerprint());
    }

    /// 黄金向量：锁定算法，一旦改动会立刻在 CI 报警。
    #[test]
    fn golden_fingerprint() {
        let banks = vec![demo_bank("math", 50, 10), demo_bank("english", 40, 8)];
        let p = paper_default(2021001, 1, b"golden-salt", &banks);
        let fp = p.fingerprint();
        assert_eq!(fp.len(), 64, "blake3 hex 长度");
        assert_eq!(fp, GOLDEN_FP, "抽题算法变更需同步更新黄金向量并说明原因");
    }

    /// 黄金向量值。**修改前请确认这是有意为之**：改它意味着所有已发出的
    /// 试卷在重跑时会变题。
    ///
    /// v2 变更记录（2026-10-22）：fingerprint 加入长度前缀以消除拼接歧义，
    /// 因此向量值随之改变。此前无真实试卷发出，变更无影响。
    ///
    /// 取值条件：student=2021001, session=1, salt="golden-salt",
    /// banks=[math(50 题, pick 10), english(40 题, pick 8)]，使用题库自带 pick。
    const GOLDEN_FP: &str = "6e429e5e0cf9d03a4ec340c949a6c6e7bbeb0de3043da70e27b0674a2861f971";
}
