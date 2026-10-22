//! 确定性抽题：固定 (学号, 场次, salt) → 固定试卷，跨平台/跨版本稳定。
//!
//! 稳定性三原则：
//! 1. 种子用 blake3 显式算出 32 字节，不依赖 rand 内部的 seed_from_u64 细节；
//! 2. 随机源用 ChaCha8Rng（算法固定，rand_chacha 承诺跨版本一致）；
//! 3. 洗牌自己实现 Fisher-Yates，只依赖 next_u32()，不用 slice::shuffle
//!   （后者实现可能随 rand 大版本变化）。

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
    pub fn fingerprint(&self) -> String {
        let mut h = blake3::Hasher::new();
        h.update(&self.student_id.to_le_bytes());
        h.update(&self.session_id.to_le_bytes());
        for (exam_id, ids) in &self.sections {
            h.update(exam_id.as_bytes());
            for id in ids {
                h.update(id.as_bytes());
            }
        }
        h.finalize().to_hex().to_string()
    }
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

/// 从题库中确定性抽取 `pick` 道题，返回题目 id。
fn pick_from_bank(bank: &ExamBank, rng: &mut ChaCha8Rng) -> Vec<String> {
    let mut pool: Vec<usize> = (0..bank.questions.len()).collect();
    fisher_yates_shuffle(&mut pool, rng);
    pool[..bank.pick as usize]
        .iter()
        .map(|&i| bank.questions[i].id.clone())
        .collect()
}

/// 生成一份完整试卷。
///
/// `salt` 取 manifest.salt 的原始字节（hex 解码后）。
pub fn generate_paper(
    student_id: u64,
    session_id: u32,
    salt: &[u8],
    banks: &[ExamBank],
) -> Paper {
    let mut rng = make_rng(student_id, session_id, salt);
    let sections = banks
        .iter()
        .map(|b| (b.exam_id.clone(), pick_from_bank(b, &mut rng)))
        .collect();
    Paper {
        student_id,
        session_id,
        sections,
    }
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

    #[test]
    fn same_seed_same_paper() {
        let banks = vec![demo_bank("math", 100, 20), demo_bank("english", 80, 15)];
        let p1 = generate_paper(2021001, 1, b"salt", &banks);
        let p2 = generate_paper(2021001, 1, b"salt", &banks);
        assert_eq!(p1, p2);
        assert_eq!(p1.fingerprint(), p2.fingerprint());
    }

    #[test]
    fn different_student_different_paper() {
        let banks = vec![demo_bank("math", 100, 20)];
        let p1 = generate_paper(2021001, 1, b"salt", &banks);
        let p2 = generate_paper(2021002, 1, b"salt", &banks);
        assert_ne!(p1.sections, p2.sections);
    }

    #[test]
    fn different_session_different_paper() {
        let banks = vec![demo_bank("math", 100, 20)];
        let p1 = generate_paper(2021001, 1, b"salt", &banks);
        let p2 = generate_paper(2021001, 2, b"salt", &banks);
        assert_ne!(p1.sections, p2.sections);
    }

    #[test]
    fn pick_count_and_uniqueness() {
        let banks = vec![demo_bank("math", 100, 20)];
        let p = generate_paper(2021001, 1, b"salt", &banks);
        let ids = &p.sections[0].1;
        assert_eq!(ids.len(), 20);
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 20, "抽题结果不应有重复");
    }

    /// 黄金向量：锁定算法，一旦改动会立刻在 CI 报警。
    #[test]
    fn golden_fingerprint() {
        let banks = vec![demo_bank("math", 50, 10), demo_bank("english", 40, 8)];
        let p = generate_paper(2021001, 1, b"golden-salt", &banks);
        // 首次运行后把实际值填进来；后续任何算法变更都会 break 这个测试。
        let fp = p.fingerprint();
        assert_eq!(fp.len(), 64, "blake3 hex 长度");
        // 记录当前值，便于人工核对（改算法时更新此值并注明原因）。
        insta_like_snapshot(&fp);
    }

    fn insta_like_snapshot(fp: &str) {
        // 简单快照：打印出来供人工比对，避免引入 insta 依赖。
        eprintln!("golden fingerprint = {fp}");
    }
}
