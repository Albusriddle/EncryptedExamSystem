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

/// 试卷中的一道题（题干快照）。
///
/// **类型层面不含 `answer` 字段**——这是刻意的设计：
/// 答案不可能通过这个类型泄漏，比"运行时记得过滤"可靠得多。
/// 即使上游 `Question.answer` 未被剥离，也无法进入试卷。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PaperItem {
    pub id: String,
    /// 题型：single / multiple / judge / fill / essay
    pub qtype: String,
    pub stem: String,
    /// 旧格式的纯文本选项；新格式走 `body`。
    pub options: Vec<String>,
    /// 题目正文形态（选项 / 代码模板 / 逐行代码）。
    pub body: crate::model::QuestionBody,
    pub score: u32,
}

impl From<&crate::model::Question> for PaperItem {
    fn from(q: &crate::model::Question) -> Self {
        use crate::model::{ChoiceOption, QuestionBody};

        // 旧题库只有 `options: ["a","b"]`，没有 body —— 在这里补成带键的
        // Options，让客户端只需处理一种形态。
        let body = match &q.body {
            QuestionBody::Text if !q.options.is_empty() => QuestionBody::Options {
                options: q
                    .options
                    .iter()
                    .enumerate()
                    .map(|(i, text)| ChoiceOption {
                        key: crate::model::option_label(i),
                        text: text.clone(),
                    })
                    .collect(),
            },
            other => other.clone(),
        };

        Self {
            id: q.id.clone(),
            qtype: q.qtype.clone(),
            stem: q.stem.clone(),
            options: q.options.clone(),
            body,
            score: q.score,
            // 注意：`q.answer` / `q.judge` 被有意忽略，本结构没有对应字段可放。
        }
    }
}

/// 一个大题下的一个部分。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PaperPart {
    pub part_id: String,
    pub title: String,
    pub items: Vec<PaperItem>,
}

/// 试卷的一个大题（含可选的部分）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PaperSection {
    /// 内部稳定标识（用于指纹）。**不要用 title 做指纹**——
    /// title 是给人看的，改名不应让已发试卷的溯源记录失效。
    pub exam_id: String,
    /// 学生可见的大题名（如「第1大题」）。
    pub title: String,
    /// 扁平题列表（兼容旧 GUI/CLI；等于 parts 的拼接）。
    pub items: Vec<PaperItem>,
    /// 新两层模式：部分列表；旧包为空。
    #[serde(default)]
    pub parts: Vec<PaperPart>,
}

impl PaperSection {
    /// 本部分总分。
    pub fn total_score(&self) -> u32 {
        self.items.iter().map(|i| i.score).sum()
    }
}

/// 抽题结果：每门考试选中的题目（含题干）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Paper {
    pub student_id: u64,
    pub session_id: u32,
    pub sections: Vec<PaperSection>,
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
        for sec in &self.sections {
            // 用 exam_id 而非 title：title 可能被改写（如加「A卷」后缀），
            // 而指纹必须锚定稳定标识。
            write_len_prefixed(&mut h, sec.exam_id.as_bytes());
            // 新两层模式把部分 ID 也纳入指纹；旧包 parts 为空，指纹不变。
            if !sec.parts.is_empty() {
                h.update(b"parts");
                for part in &sec.parts {
                    write_len_prefixed(&mut h, part.part_id.as_bytes());
                    h.update(&(part.items.len() as u32).to_le_bytes());
                    for item in &part.items {
                        write_len_prefixed(&mut h, item.id.as_bytes());
                    }
                }
            }
            h.update(&(sec.items.len() as u32).to_le_bytes());
            // 只哈希题目 ID：指纹语义是"选了哪几题"，
            // 题干错别字修正不应改变试卷指纹（否则溯源记录会失效）。
            for item in &sec.items {
                write_len_prefixed(&mut h, item.id.as_bytes());
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
    #[error("题库 {exam_id} 有 {count} 道题没有有效难度（1–255），无法按平均难度抽题")]
    DifficultyMissing { exam_id: String, count: usize },
    #[error("题库 {exam_id} 抽 {pick} 题无法满足平均难度 {lo}–{hi}")]
    NoFeasibleDifficulty {
        exam_id: String,
        pick: u32,
        lo: u8,
        hi: u8,
    },
    #[error("大题 {title} 引用的题库 {file} 不存在")]
    SectionBankMissing { title: String, file: String },
    #[error("大题 {title} 要抽 {want} 题，但题库 {file} 难度 {difficulty} 只有 {available} 题")]
    SectionNotEnough {
        title: String,
        file: String,
        want: u32,
        difficulty: u8,
        available: usize,
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
    range: Option<(u8, u8)>,
) -> Result<Vec<PaperItem>, PickError> {
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

    let chosen = match range {
        None => pool[..n].to_vec(),
        Some((lo, hi)) => choose_with_range(bank, n, &pool, lo, hi)?,
    };
    Ok(chosen
        .iter()
        .map(|&i| PaperItem::from(&bank.questions[i]))
        .collect())
}

/// 是否存在 k 个难度、平均值落在 `[lo, hi]` 的子集（精确判定）。
///
/// 只看 min/max 会放过 `{1,3}` 抽 1 题、区间 2–2 这种"没有可行组合"的配置。
pub fn feasible_average(diffs: &[u8], k: usize, lo: u8, hi: u8) -> bool {
    if k == 0 || k > diffs.len() {
        return false;
    }
    let lo_sum = (k as f32 * lo as f32).ceil() as usize;
    let hi_sum = ((k as f32 * hi as f32).floor() as usize).min(k * 255);
    if lo_sum > hi_sum {
        return false;
    }
    let words = hi_sum / 64 + 1;
    let mut dp = vec![vec![0u64; words]; k + 1];
    dp[0][0] = 1;

    // 按字移位：每次 `dp[j] |= dp[j-1] << d` 是 O(words)，
    // 比逐位遍历 hi_sum 快约 64 倍（构建期大题库不会卡住）。
    for &d in diffs {
        let d = d as usize;
        if d > hi_sum {
            continue;
        }
        let word_shift = d / 64;
        let bit_shift = d % 64;
        for j in (1..=k).rev() {
            let (before, after) = dp.split_at_mut(j);
            let prev = &before[j - 1];
            let cur = &mut after[0];
            for w in (0..words).rev() {
                let mut v = 0u64;
                if w >= word_shift {
                    v |= prev[w - word_shift] << bit_shift;
                    if bit_shift > 0 && w > word_shift {
                        v |= prev[w - word_shift - 1] >> (64 - bit_shift);
                    }
                }
                cur[w] |= v;
            }
        }
    }

    (lo_sum..=hi_sum).any(|sum| dp[k][sum / 64] >> (sum % 64) & 1 == 1)
}

/// 在洗牌后的池子里挑 `n` 道，使平均难度落在 `[lo, hi]`。
///
/// 用 0/1 背包位集 DP：`dp[j]` 是"用 j 道题能达到的难度和"位集，
/// 再按 parent 表回溯出具体题目。判定是精确的（只要存在就一定能找到），
/// 组合过多时退化为"按难度排序的滑窗"（见 `window_fallback`）。
fn choose_with_range(
    bank: &ExamBank,
    n: usize,
    order: &[usize],
    lo: u8,
    hi: u8,
) -> Result<Vec<usize>, PickError> {
    let missing = bank
        .questions
        .iter()
        .filter(|q| q.difficulty.is_none_or(|d| d == 0))
        .count();
    if missing > 0 {
        return Err(PickError::DifficultyMissing {
            exam_id: bank.exam_id.clone(),
            count: missing,
        });
    }
    if n == 0 {
        return Ok(Vec::new());
    }
    // 全选：平均值固定，只能看是否落在区间
    if n == order.len() {
        let sum: usize = order
            .iter()
            .map(|&i| bank.questions[i].difficulty.unwrap() as usize)
            .sum();
        let mean = sum as f32 / n as f32;
        if mean + 1e-6 < lo as f32 || mean - 1e-6 > hi as f32 {
            return Err(PickError::NoFeasibleDifficulty {
                exam_id: bank.exam_id.clone(),
                pick: n as u32,
                lo,
                hi,
            });
        }
        return Ok(order.to_vec());
    }

    let diffs: Vec<u8> = order
        .iter()
        .map(|&i| bank.questions[i].difficulty.unwrap())
        .collect();
    let lo_sum = (n as f32 * lo as f32).ceil() as usize;
    let hi_sum = ((n as f32 * hi as f32).floor() as usize).min(n * 255);
    if lo_sum > hi_sum {
        return Err(PickError::NoFeasibleDifficulty {
            exam_id: bank.exam_id.clone(),
            pick: n as u32,
            lo,
            hi,
        });
    }

    // 内存保护：parent 表 (n+1) × (hi_sum+1) 个 u16
    let cells = (n + 1).saturating_mul(hi_sum + 1);
    // 操作数保护：内层循环约 order.len() × n × hi_sum 次。
    // 大题库 + 宽区间（如 1000 题抽 150、1–255）会到几十亿次，必须回退。
    let ops = order
        .len()
        .saturating_mul(n)
        .saturating_mul(hi_sum.saturating_add(1));
    if cells > 8_000_000 || ops > 50_000_000 {
        return window_fallback(bank, n, order, &diffs, (lo, hi), (lo_sum, hi_sum));
    }

    let words = hi_sum / 64 + 1;
    let mut dp = vec![vec![0u64; words]; n + 1];
    let mut parent = vec![vec![u16::MAX; hi_sum + 1]; n + 1];
    dp[0][0] = 1;

    for (pos, &d) in diffs.iter().enumerate() {
        let d = d as usize;
        if d > hi_sum {
            continue;
        }
        let upper = n.min(pos + 1);
        for j in (1..=upper).rev() {
            for s in (d..=hi_sum).rev() {
                let prev = s - d;
                if dp[j - 1][prev / 64] >> (prev % 64) & 1 == 1
                    && dp[j][s / 64] >> (s % 64) & 1 == 0
                {
                    dp[j][s / 64] |= 1 << (s % 64);
                    parent[j][s] = pos as u16;
                }
            }
        }
    }

    // 选一个"最接近无约束均值"的可达和
    let unconstrained: usize = diffs.iter().take(n).map(|&d| d as usize).sum();
    let mut best: Option<(usize, usize)> = None;
    for s in lo_sum..=hi_sum {
        if dp[n][s / 64] >> (s % 64) & 1 == 1 {
            let dist = (s as i64 - unconstrained as i64).unsigned_abs() as usize;
            if best.is_none_or(|(bd, _)| dist < bd) {
                best = Some((dist, s));
            }
        }
    }
    let Some((_, mut sum)) = best else {
        return Err(PickError::NoFeasibleDifficulty {
            exam_id: bank.exam_id.clone(),
            pick: n as u32,
            lo,
            hi,
        });
    };

    let mut chosen = Vec::with_capacity(n);
    let mut j = n;
    while j > 0 {
        let pos = parent[j][sum] as usize;
        if pos == u16::MAX as usize {
            return Err(PickError::NoFeasibleDifficulty {
                exam_id: bank.exam_id.clone(),
                pick: n as u32,
                lo,
                hi,
            });
        }
        let idx = order[pos];
        chosen.push(idx);
        sum -= diffs[pos] as usize;
        j -= 1;
    }
    chosen.reverse();
    Ok(chosen)
}

/// 组合规模过大时的退化方案：按难度排序后滑动窗口，取均值最接近区间的窗口。
fn window_fallback(
    bank: &ExamBank,
    n: usize,
    order: &[usize],
    diffs: &[u8],
    range: (u8, u8),
    sums: (usize, usize),
) -> Result<Vec<usize>, PickError> {
    let (lo, hi) = range;
    let (lo_sum, hi_sum) = sums;
    let mut sorted: Vec<usize> = (0..order.len()).collect();
    sorted.sort_by_key(|&i| (diffs[i], i));
    let mut best: Option<(usize, usize, usize)> = None; // (distance, start, sum)
    for start in 0..=order.len().saturating_sub(n) {
        let sum: usize = sorted[start..start + n]
            .iter()
            .map(|&i| diffs[i] as usize)
            .sum();
        let inside = sum >= lo_sum && sum <= hi_sum;
        let dist = if inside {
            0
        } else if sum < lo_sum {
            lo_sum - sum
        } else {
            sum - hi_sum
        };
        if best.is_none_or(|(bd, _, _)| dist < bd) {
            best = Some((dist, start, sum));
            if dist == 0 {
                break;
            }
        }
    }
    let Some((dist, start, _)) = best else {
        return Err(PickError::NoFeasibleDifficulty {
            exam_id: bank.exam_id.clone(),
            pick: n as u32,
            lo,
            hi,
        });
    };
    let mut chosen: Vec<usize> = sorted[start..start + n].to_vec();
    let mut total: usize = chosen.iter().map(|&i| diffs[i] as usize).sum();

    // 贪心修补：从最接近的连续窗口出发，逐步换入更大/更小的题，
    // 每次只移动一格，尽量走到区间内（比直接报"不可行"更稳）。
    if total < lo_sum {
        let mut outside = start + n..sorted.len();
        while total < lo_sum {
            let Some(big) = outside.next() else { break };
            let mut pos = 0usize;
            for (idx, &i) in chosen.iter().enumerate() {
                if diffs[i] < diffs[chosen[pos]] {
                    pos = idx;
                }
            }
            let small = chosen[pos];
            total = total - diffs[small] as usize + diffs[big] as usize;
            chosen[pos] = big;
        }
    } else if total > hi_sum {
        let mut outside = (0..start).rev();
        while total > hi_sum {
            let Some(small) = outside.next() else { break };
            let mut pos = 0usize;
            for (idx, &i) in chosen.iter().enumerate() {
                if diffs[i] > diffs[chosen[pos]] {
                    pos = idx;
                }
            }
            let big = chosen[pos];
            total = total - diffs[big] as usize + diffs[small] as usize;
            chosen[pos] = small;
        }
    }

    if total < lo_sum || total > hi_sum {
        return Err(PickError::NoFeasibleDifficulty {
            exam_id: bank.exam_id.clone(),
            pick: n as u32,
            lo,
            hi,
        });
    }
    let _ = dist;
    Ok(chosen.iter().map(|&i| order[i]).collect())
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
    generate_paper_ranged(student_id, session_id, salt, banks, |exam_id, fallback| {
        (pick_for(exam_id, fallback), None)
    })
}

/// 带"平均难度区间"的抽题。`pick_for` 返回 (抽题数, 可选区间)。
pub fn generate_paper_ranged<F>(
    student_id: u64,
    session_id: u32,
    salt: &[u8],
    banks: &[ExamBank],
    pick_for: F,
) -> Result<Paper, PickError>
where
    F: Fn(&str, u32) -> (u32, Option<(u8, u8)>),
{
    let mut rng = make_rng(student_id, session_id, salt);
    let mut sections = Vec::with_capacity(banks.len());
    for b in banks {
        let (pick, range) = pick_for(&b.exam_id, b.pick);
        let ids = pick_from_bank(b, pick, &mut rng, range)?;
        sections.push(PaperSection {
            exam_id: b.exam_id.clone(),
            title: b.title.clone(),
            items: ids,
            parts: Vec::new(),
        });
    }
    Ok(Paper {
        student_id,
        session_id,
        sections,
    })
}

fn bank_for_file<'a>(banks: &'a [ExamBank], file: &str) -> Option<&'a ExamBank> {
    let base = file.rsplit('/').next().unwrap_or(file);
    let stem = base.strip_suffix(".json").unwrap_or(base);
    let full = file
        .strip_suffix(".json")
        .unwrap_or(file)
        .replace(['/', '_'], "-");
    banks.iter().find(|b| {
        b.exam_id == stem
            || b.exam_id == base
            || b.exam_id == full
            || format!("{}.json", b.exam_id) == base
    })
}

/// 按“大题”生成试卷：每个 section 固定题库、题数、精确难度，顺序即卷面顺序。
pub fn generate_paper_sections(
    student_id: u64,
    session_id: u32,
    salt: &[u8],
    banks: &[ExamBank],
    sections: &[crate::model::Section],
) -> Result<Paper, PickError> {
    let mut rng = make_rng(student_id, session_id, salt);
    let mut used: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(sections.len());
    for sec in sections {
        let bank =
            bank_for_file(banks, &sec.file).ok_or_else(|| PickError::SectionBankMissing {
                title: sec.title.clone(),
                file: sec.file.clone(),
            })?;
        let mut candidates: Vec<usize> = bank
            .questions
            .iter()
            .enumerate()
            .filter(|(_, q)| q.difficulty == Some(sec.difficulty))
            .map(|(i, _)| i)
            .collect();
        if candidates.len() < sec.count as usize {
            return Err(PickError::SectionNotEnough {
                title: sec.title.clone(),
                file: sec.file.clone(),
                want: sec.count,
                difficulty: sec.difficulty,
                available: candidates.len(),
            });
        }
        fisher_yates_shuffle(&mut candidates, &mut rng);
        let mut items = Vec::with_capacity(sec.count as usize);
        for &i in &candidates {
            if used.insert((bank.exam_id.clone(), bank.questions[i].id.clone())) {
                items.push(PaperItem::from(&bank.questions[i]));
                if items.len() == sec.count as usize {
                    break;
                }
            }
        }
        if items.len() != sec.count as usize {
            return Err(PickError::SectionNotEnough {
                title: sec.title.clone(),
                file: sec.file.clone(),
                want: sec.count,
                difficulty: sec.difficulty,
                available: items.len(),
            });
        }
        out.push(PaperSection {
            exam_id: sec.section_id.clone(),
            title: sec.title.clone(),
            items,
            parts: Vec::new(),
        });
    }
    Ok(Paper {
        student_id,
        session_id,
        sections: out,
    })
}

/// 按“大题 → 部分”生成试卷：每个部分固定精确难度，顺序即卷面顺序。
pub fn generate_paper_parts(
    student_id: u64,
    session_id: u32,
    salt: &[u8],
    banks: &[ExamBank],
    sections: &[crate::model::Section],
) -> Result<Paper, PickError> {
    let mut rng = make_rng(student_id, session_id, salt);
    let mut used: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(sections.len());
    for sec in sections {
        let bank =
            bank_for_file(banks, &sec.file).ok_or_else(|| PickError::SectionBankMissing {
                title: sec.title.clone(),
                file: sec.file.clone(),
            })?;
        let mut parts = Vec::new();
        let mut flat = Vec::new();
        for part in sec.effective_parts() {
            let mut candidates: Vec<usize> = bank
                .questions
                .iter()
                .enumerate()
                .filter(|(_, q)| q.difficulty == Some(part.difficulty))
                .map(|(i, _)| i)
                .collect();
            fisher_yates_shuffle(&mut candidates, &mut rng);
            let mut items = Vec::with_capacity(part.count as usize);
            for &i in &candidates {
                if used.insert((bank.exam_id.clone(), bank.questions[i].id.clone())) {
                    items.push(PaperItem::from(&bank.questions[i]));
                    if items.len() == part.count as usize {
                        break;
                    }
                }
            }
            if items.len() != part.count as usize {
                return Err(PickError::SectionNotEnough {
                    title: format!("{} / {}", sec.title, part.title),
                    file: sec.file.clone(),
                    want: part.count,
                    difficulty: part.difficulty,
                    available: items.len(),
                });
            }
            flat.extend(items.clone());
            parts.push(PaperPart {
                part_id: part.part_id,
                title: part.title,
                items,
            });
        }
        out.push(PaperSection {
            exam_id: sec.section_id.clone(),
            title: sec.title.clone(),
            items: flat,
            parts,
        });
    }
    Ok(Paper {
        student_id,
        session_id,
        sections: out,
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
                    body: Default::default(),
                    kp: None,
                    difficulty: None,
                    answer: Some(serde_json::json!("A")),
                    judge: None,
                    score: 2,
                })
                .collect(),
        }
    }

    fn difficulty_bank() -> ExamBank {
        let mut b = demo_bank("math", 12, 1);
        for (i, q) in b.questions.iter_mut().enumerate() {
            q.difficulty = Some((i % 3 + 1) as u8);
        }
        b
    }

    #[test]
    fn sections_keep_order_title_and_exact_difficulty() {
        let bank = difficulty_bank();
        let sections = vec![
            crate::model::Section {
                section_id: "s1".into(),
                title: "第1大题".into(),
                file: "math.json".into(),
                count: 2,
                difficulty: 1,
                parts: vec![],
            },
            crate::model::Section {
                section_id: "s2".into(),
                title: "第2大题".into(),
                file: "math.json".into(),
                count: 2,
                difficulty: 1,
                parts: vec![],
            },
        ];
        let paper =
            generate_paper_sections(1, 1, b"salt", std::slice::from_ref(&bank), &sections).unwrap();
        assert_eq!(paper.sections.len(), 2);
        assert_eq!(paper.sections[0].exam_id, "s1");
        assert_eq!(paper.sections[1].title, "第2大题");
        let mut ids = std::collections::HashSet::new();
        for sec in &paper.sections {
            for item in &sec.items {
                assert!(ids.insert(item.id.clone()), "同一题不能出现在两个大题");
            }
        }
        assert_eq!(paper.sections[0].items.len(), 2);
    }

    #[test]
    fn parts_flatten_matches_items() {
        let bank = difficulty_bank();
        let sections = vec![crate::model::Section {
            section_id: "s1".into(),
            title: "第1大题".into(),
            file: "math.json".into(),
            count: 4,
            difficulty: 1,
            parts: vec![
                crate::model::SectionPart {
                    part_id: "s1p1".into(),
                    title: "第1部分".into(),
                    count: 2,
                    difficulty: 1,
                },
                crate::model::SectionPart {
                    part_id: "s1p2".into(),
                    title: "第2部分".into(),
                    count: 2,
                    difficulty: 2,
                },
            ],
        }];
        let paper = generate_paper_parts(1, 1, b"salt", &[bank], &sections).unwrap();
        let sec = &paper.sections[0];
        assert_eq!(sec.parts.len(), 2);
        assert_eq!(
            sec.items.len(),
            sec.parts.iter().map(|p| p.items.len()).sum::<usize>()
        );
    }

    /// 旧题库只有 `options: ["A","B",…]`，没有 body。
    /// 进试卷时必须自动补成带键的 Options，否则学生端只剩一串裸文本。
    #[test]
    fn legacy_options_become_labelled_body() {
        let bank = demo_bank("m", 1, 1);
        let item = PaperItem::from(&bank.questions[0]);
        match item.body {
            crate::model::QuestionBody::Options { options } => {
                let keys: Vec<_> = options.iter().map(|o| o.key.as_str()).collect();
                assert_eq!(keys, ["A", "B", "C", "D"]);
                assert_eq!(options[0].text, "A");
            }
            other => panic!("旧题库应被补成 Options，实际 {other:?}"),
        }
    }

    /// 三种正文形态都要能序列化/反序列化，且 `text` 是缺省值。
    #[test]
    fn body_kinds_round_trip() {
        let json = r#"{
            "exam_id": "m", "title": "t", "pick": 3,
            "questions": [
                {"id": "1", "type": "single", "stem": "s", "options": ["甲", "乙"]},
                {"id": "2", "type": "fill", "stem": "s",
                 "body": {"kind": "template", "template": "x = ____①____;", "blank_count": 1}},
                {"id": "3", "type": "fix", "stem": "s",
                 "body": {"kind": "code", "code": ["a", "b"], "blank_count": 1}}
            ]
        }"#;
        let bank: crate::model::ExamBank = serde_json::from_str(json).unwrap();
        assert_eq!(bank.questions[0].body, crate::model::QuestionBody::Text);
        match &bank.questions[1].body {
            crate::model::QuestionBody::Template {
                template,
                blank_count,
            } => {
                assert!(template.contains("____①____"));
                assert_eq!(*blank_count, 1);
            }
            other => panic!("应为 Template，实际 {other:?}"),
        }
        match &bank.questions[2].body {
            crate::model::QuestionBody::Code { code, blank_count } => {
                assert_eq!(code.len(), 2);
                assert_eq!(*blank_count, 1);
            }
            other => panic!("应为 Code，实际 {other:?}"),
        }

        // 再序列化回去，形态不能丢
        let back: crate::model::ExamBank =
            serde_json::from_str(&serde_json::to_string(&bank).unwrap()).unwrap();
        assert_eq!(back.questions[1].body, bank.questions[1].body);
        assert_eq!(back.questions[2].body, bank.questions[2].body);
    }

    /// 测试用的最小 PaperSection。
    fn section(exam_id: &str, items: Vec<PaperItem>) -> PaperSection {
        PaperSection {
            exam_id: exam_id.into(),
            title: format!("{exam_id} 科目"),
            items,
            parts: Vec::new(),
        }
    }

    /// 测试用的最小 PaperItem（只有 id 有意义）。
    fn item(id: &str) -> PaperItem {
        PaperItem {
            id: id.into(),
            qtype: "single".into(),
            stem: format!("题干 {id}"),
            options: vec![],
            body: Default::default(),
            score: 2,
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
        let items = &p.sections[0].items;
        assert_eq!(items.len(), 20);
        let mut ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 20, "抽题结果不应有重复");
    }

    /// 回归：清单里设置的抽题数必须真正生效（曾经被题库自带 pick 覆盖）。
    #[test]
    fn explicit_pick_overrides_bank_default() {
        let banks = vec![demo_bank("math", 100, 3)];
        let p = generate_paper(2021001, 1, b"salt", &banks, |_, _| 25).unwrap();
        assert_eq!(
            p.sections[0].items.len(),
            25,
            "应遵循显式 pick 而非题库默认值"
        );
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
            sections: vec![section("ab", vec![item("c")])],
        };
        let b = Paper {
            student_id: 1,
            session_id: 1,
            sections: vec![section("a", vec![item("bc")])],
        };
        assert_ne!(a.fingerprint(), b.fingerprint(), "拼接歧义必须被消除");

        // 题目 id 列表的分组也要有边界
        let c = Paper {
            student_id: 1,
            session_id: 1,
            sections: vec![section("x", vec![item("ab"), item("c")])],
        };
        let d = Paper {
            student_id: 1,
            session_id: 1,
            sections: vec![section("x", vec![item("a"), item("bc")])],
        };
        assert_ne!(c.fingerprint(), d.fingerprint());
    }

    /// 试卷必须携带题干（此前只返回 ID，学生看不到题目）。
    #[test]
    fn paper_items_carry_stem_and_options() {
        let banks = vec![demo_bank("math", 20, 3)];
        let p = paper_default(2021001, 1, b"salt", &banks);
        let items = &p.sections[0].items;
        assert_eq!(items.len(), 3);
        for it in items {
            assert!(!it.stem.is_empty(), "题干不能为空");
            assert!(!it.qtype.is_empty(), "题型不能为空");
            assert_eq!(it.options.len(), 4, "选项应被带出");
            assert!(it.score > 0);
        }
    }

    /// 指纹只依赖题目 ID：改题干不应改变指纹（溯源记录保持有效）。
    #[test]
    fn fingerprint_ignores_stem_changes() {
        let banks = vec![demo_bank("math", 20, 3)];
        let p1 = paper_default(2021001, 1, b"salt", &banks);
        let mut p2 = p1.clone();
        for sec in &mut p2.sections {
            for it in &mut sec.items {
                it.stem = format!("【改过的题干】{}", it.stem);
            }
        }
        assert_eq!(p1.fingerprint(), p2.fingerprint(), "题干变化不应影响指纹");

        // 但换题必须影响指纹
        let mut p3 = p1.clone();
        p3.sections[0].items[0].id = "different-id".into();
        assert_ne!(p1.fingerprint(), p3.fingerprint(), "换题必须改变指纹");
    }

    /// 答案绝不能进入试卷：PaperItem 没有 answer 字段，
    /// 即使源题库带着答案也带不出来（编译期保证）。
    #[test]
    fn paper_items_never_expose_answer() {
        let mut bank = demo_bank("math", 10, 3);
        // 源题库故意保留答案
        for q in &mut bank.questions {
            q.answer = Some(serde_json::json!("SECRET_ANSWER"));
            q.judge = Some(serde_json::json!({"harness": "SECRET_JUDGE"}));
        }
        let p = paper_default(2021001, 1, b"salt", &[bank]);
        let json = serde_json::to_string(&p.sections).unwrap();
        assert!(
            !json.contains("SECRET_ANSWER") && !json.contains("SECRET_JUDGE"),
            "试卷序列化结果中出现了答案或判分材料"
        );
        assert!(!json.contains("answer"), "试卷结构不应含 answer 字段");
    }

    /// 试卷部分必须携带学生可见的科目名（title），而非只有 exam_id。
    #[test]
    fn section_carries_title_and_score() {
        let banks = vec![demo_bank("math", 20, 3)];
        let p = paper_default(2021001, 1, b"salt", &banks);
        let sec = &p.sections[0];
        assert_eq!(sec.exam_id, "math");
        assert_eq!(sec.title, "math"); // demo_bank 里 title == exam_id
        assert!(!sec.title.is_empty(), "科目名不能为空");
        assert_eq!(sec.total_score(), 3 * 2, "总分应为 3 题 × 2 分");
    }

    /// 指纹锚定 exam_id 而非 title：改科目名不应改变指纹
    /// （否则「高等数学」改成「高等数学（A卷）」会让溯源记录失效）。
    #[test]
    fn fingerprint_ignores_title_changes() {
        let banks = vec![demo_bank("math", 20, 3)];
        let p1 = paper_default(2021001, 1, b"salt", &banks);
        let mut p2 = p1.clone();
        p2.sections[0].title = "高等数学（A卷）".into();
        assert_eq!(p1.fingerprint(), p2.fingerprint(), "改 title 不应影响指纹");

        // 但改 exam_id 必须影响
        let mut p3 = p1.clone();
        p3.sections[0].exam_id = "different".into();
        assert_ne!(
            p1.fingerprint(),
            p3.fingerprint(),
            "改 exam_id 必须改变指纹"
        );
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

    fn bank_with_difficulty(diffs: &[u8]) -> ExamBank {
        let mut b = demo_bank("m", diffs.len(), diffs.len() as u32);
        for (q, d) in b.questions.iter_mut().zip(diffs) {
            q.difficulty = Some(*d);
        }
        b
    }

    #[test]
    fn ranged_pick_respects_average_and_is_deterministic() {
        let diffs: Vec<u8> = (1..=10).collect();
        let bank = bank_with_difficulty(&diffs);
        let salt = b"salt";

        for student in [1001_u64, 1002, 1003] {
            let paper =
                generate_paper_ranged(student, 1, salt, std::slice::from_ref(&bank), |_, _| {
                    (4, Some((4, 6)))
                })
                .expect("应能抽到满足区间的题");
            let sum: f32 = paper.sections[0]
                .items
                .iter()
                .map(|i| {
                    let q = bank.questions.iter().find(|q| q.id == i.id).unwrap();
                    q.difficulty.unwrap() as f32
                })
                .sum();
            let mean = sum / 4.0;
            assert!((4.0..=6.0).contains(&mean), "平均难度 {mean} 不在 4–6");

            let again =
                generate_paper_ranged(student, 1, salt, std::slice::from_ref(&bank), |_, _| {
                    (4, Some((4, 6)))
                })
                .unwrap();
            assert_eq!(
                paper.fingerprint(),
                again.fingerprint(),
                "同学生必须同一份卷"
            );
        }
    }

    #[test]
    fn ranged_pick_rejects_missing_difficulty() {
        let bank = demo_bank("m", 5, 2); // difficulty 全是 None
        let err = generate_paper_ranged(1, 1, b"s", &[bank], |_, _| (2, Some((2, 3)))).unwrap_err();
        assert!(matches!(err, PickError::DifficultyMissing { .. }));
    }

    #[test]
    fn ranged_pick_rejects_infeasible_range() {
        let bank = bank_with_difficulty(&[1, 1, 1, 1]);
        let err = generate_paper_ranged(1, 1, b"s", &[bank], |_, _| (2, Some((4, 5)))).unwrap_err();
        assert!(matches!(err, PickError::NoFeasibleDifficulty { .. }));
    }

    #[test]
    fn feasible_average_rejects_gap() {
        // {1,3} 抽 1 题、区间 2–2：min/max 看着可行，实际没有组合
        assert!(!feasible_average(&[1, 3], 1, 2, 2));
        assert!(feasible_average(&[1, 3], 1, 1, 1));
        assert!(feasible_average(&[1, 3], 2, 2, 2));
    }
}
