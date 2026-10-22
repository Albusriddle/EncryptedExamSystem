//! exam-client：学生端（合并版单 exe）。
//!
//! 流程（严格按此顺序，门禁必须在派生密钥之前）：
//!
//! ```text
//! 读自身 payload → 解 header（K_manifest）
//!   → 【时间门禁】多源取时 → 找时间窗内的场次
//!   → Argon2id(学号, 场次, salt) → 按 tag 定位信封 → 解出会话密钥
//!   → 解密题库 → 确定性抽题 → 展示试卷
//! ```
//!
//! 骨架版用命令行驱动；M4 换成 egui 输入窗，主干逻辑不变。

use anyhow::{Context, Result};
use clap::Parser;

use common::container::{self, Header, SessionHeader};
use common::crypto;
use common::picker;
use common::timegate::{self, HttpsDateSource, TimeSource};

#[derive(Parser)]
#[command(name = "exam-client", version, about = "保密考试系统学生端")]
struct Cli {
    /// 学号
    #[arg(long)]
    student: u64,
    /// 主密钥。优先 --master-file 或环境变量 EXAM_MASTER
    #[arg(long)]
    master: Option<String>,
    /// 从文件读取主密钥（64-hex）
    #[arg(long, conflicts_with = "master")]
    master_file: Option<String>,
    /// 自解压包路径（默认读自身）
    #[arg(long)]
    package: Option<String>,
    /// **跳过时间门禁**（仅开发调试用，会在 stderr 显著警告）
    #[arg(long, hide = true)]
    skip_timegate: bool,
    /// 指定场次（正常流程由时间门禁自动判定）
    #[arg(long)]
    session: Option<u32>,
}

/// 取主密钥：--master-file > --master > 环境变量 EXAM_MASTER。
fn resolve_master(master: Option<String>, master_file: Option<String>) -> Result<String> {
    if let Some(path) = master_file {
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("读取密钥文件 {path}"))?;
        return Ok(text.trim().to_string());
    }
    if let Some(m) = master {
        return Ok(m);
    }
    if let Ok(v) = std::env::var("EXAM_MASTER")
        && !v.trim().is_empty()
    {
        return Ok(v.trim().to_string());
    }
    anyhow::bail!("缺少主密钥：请用 --master-file、--master 或环境变量 EXAM_MASTER 提供")
}

/// 门禁结果：命中的场次 + 网络时间信息。
struct GateResult {
    session: SessionHeader,
    now: Option<timegate::NetworkTime>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // 1. 读自身 payload。
    let exe_path = match &cli.package {
        Some(p) => p.clone(),
        None => std::env::current_exe()
            .context("获取自身路径")?
            .to_string_lossy()
            .into_owned(),
    };
    let bytes = std::fs::read(&exe_path).with_context(|| format!("读取 {exe_path}"))?;
    let payload = container::extract_payload(&bytes)
        .context("未找到内嵌 payload。请用 --package 指向 builder 生成的 exe")?;

    // 2. 解 header（正式版主密钥由 stub 现场拼装）。
    let master_hex = resolve_master(cli.master.clone(), cli.master_file.clone())?;
    let master = crypto::master_from_hex(&master_hex).context("解析主密钥")?;
    let k_manifest = crypto::hkdf_derive(&master, b"", "manifest");
    let (header, blob_area) = container::parse_header(payload, &k_manifest)
        .context("解析 header 失败（主密钥错误？）")?;
    let salt = hex::decode(&header.salt).context("header.salt hex 解码")?;

    // 3. 【时间门禁】必须在派生入场密钥之前。
    let gate = check_time_gate(&header, &cli)?;

    // 4. 解锁：Argon2id(学号, 场次, salt) → tag → 信封。
    let unlocked =
        container::try_unlock_session(&gate.session, &header, blob_area, cli.student, &salt)
            .context("解锁失败（包损坏或 KDF 参数异常）")?
            .with_context(|| {
                format!(
                    "学号 {} 不在场次 {} 的名单中",
                    cli.student, gate.session.session_id
                )
            })?;

    // 5. 解密题库。
    let data = container::decrypt_data(&unlocked.session_secret, &salt, &unlocked.data_ct)
        .context("解密题库失败")?;

    // 6. 确定性抽题。
    let paper = picker::generate_paper(
        cli.student,
        unlocked.session_id,
        &salt,
        &data.banks,
        |exam_id, fallback| data.pick_for(exam_id, fallback),
    )
    .context("抽题失败")?;

    // 7. 展示。
    println!("✔ 解锁成功");
    println!("  学号: {}", cli.student);
    println!("  场次: {}", unlocked.session_id);
    println!("  时间窗: {} ~ {}", unlocked.start, unlocked.end);
    if let Some(t) = &gate.now {
        println!(
            "  网络时间: {} UTC（{} 个源，分歧 {} 秒）",
            timegate::format_unix_utc(t.unix),
            t.sources,
            t.spread
        );
        if t.local_skew_secs.abs() > 300 {
            println!(
                "  ⚠ 本机时钟与网络时间相差 {} 秒，判定以网络时间为准",
                t.local_skew_secs
            );
        }
    } else {
        println!("  ⚠ 已跳过时间门禁（开发模式）");
    }
    println!("  试卷指纹: {}", paper.fingerprint());
    for (exam_id, ids) in &paper.sections {
        println!("  [{exam_id}] 抽中 {} 题: {:?}", ids.len(), ids);
    }
    Ok(())
}

/// 时间门禁：取网络时间 → 选场次。
fn check_time_gate(header: &Header, cli: &Cli) -> Result<GateResult> {
    if cli.skip_timegate {
        eprintln!("⚠⚠⚠ 警告：已跳过时间门禁，仅限开发调试！⚠⚠⚠");
        let session = pick_session(header, cli.session, None)?;
        return Ok(GateResult { session, now: None });
    }

    let sources = build_time_sources();
    let now = timegate::fetch_network_time(&sources, 2, 300).map_err(|e| {
        anyhow::anyhow!(
            "无法获取可信的网络时间：{e}\n\
             请检查网络连接。为避免误判，本系统在取时失败时**拒绝解锁**。"
        )
    })?;

    let session = pick_session(header, cli.session, Some(now.unix))?;
    Ok(GateResult {
        session,
        now: Some(now),
    })
}

/// 组装时间源。默认用三个国内大站（只要"能上网"就通）。
fn build_time_sources() -> Vec<Box<dyn TimeSource>> {
    vec![
        Box::new(HttpsDateSource::new("https://www.baidu.com", "baidu")),
        Box::new(HttpsDateSource::new("https://www.aliyun.com", "aliyun")),
        Box::new(HttpsDateSource::new("https://www.tencent.com", "tencent")),
    ]
}

/// 按时间窗选场次。
fn pick_session(header: &Header, explicit: Option<u32>, now: Option<u64>) -> Result<SessionHeader> {
    if let Some(id) = explicit {
        return header
            .sessions
            .iter()
            .find(|s| s.session_id == id)
            .cloned()
            .with_context(|| format!("场次 {id} 不存在"));
    }
    let now = now.context("内部错误：未指定场次时必须有网络时间")?;

    let mut candidates = Vec::new();
    for s in &header.sessions {
        let (start, end) = match (parse_rfc3339(&s.start), parse_rfc3339(&s.end)) {
            (Some(a), Some(b)) => (a, b),
            _ => continue,
        };
        if now >= start && now <= end {
            candidates.push(s.clone());
        }
    }

    match candidates.len() {
        0 => {
            let mut starts: Vec<u64> = header
                .sessions
                .iter()
                .filter_map(|s| parse_rfc3339(&s.start))
                .collect();
            starts.sort_unstable();
            let msg = if let Some(&first) = starts.first() {
                if now < first {
                    format!(
                        "考试尚未开始。当前网络时间 {}，最早一场开始于 {}。",
                        timegate::format_unix_utc(now),
                        timegate::format_unix_utc(first)
                    )
                } else {
                    format!(
                        "当前网络时间 {}，不在任何考试时间窗内（考试可能已结束）。",
                        timegate::format_unix_utc(now)
                    )
                }
            } else {
                "包内没有任何场次。".to_string()
            };
            anyhow::bail!("{msg}")
        }
        1 => Ok(candidates.remove(0)),
        _ => {
            eprintln!(
                "⚠ 注意：有 {} 个场次时间窗重叠，取 session_id 最小的一场。",
                candidates.len()
            );
            candidates.sort_by_key(|s| s.session_id);
            Ok(candidates.remove(0))
        }
    }
}

/// 解析 RFC3339（支持 `2026-12-20T09:00:00+08:00` 与 `...Z`）。
fn parse_rfc3339(s: &str) -> Option<u64> {
    let s = s.trim();
    let (date_part, rest) = s.split_once('T').or_else(|| s.split_once(' '))?;
    let d: Vec<&str> = date_part.split('-').collect();
    if d.len() != 3 {
        return None;
    }
    let (y, mo, da): (i64, i64, i64) = (d[0].parse().ok()?, d[1].parse().ok()?, d[2].parse().ok()?);

    let (time_part, offset_secs) = if let Some(t) = rest.strip_suffix('Z') {
        (t, 0i64)
    } else if let Some(idx) = rest.rfind(['+', '-']) {
        let (t, off) = rest.split_at(idx);
        let sign = if off.starts_with('-') { -1 } else { 1 };
        let o = &off[1..];
        let (oh, om) = o.split_once(':').unwrap_or((o, "0"));
        let secs = sign * (oh.parse::<i64>().ok()? * 3600 + om.parse::<i64>().ok()? * 60);
        (t, secs)
    } else {
        (rest, 0)
    };

    let t: Vec<&str> = time_part.split(':').collect();
    if t.len() < 2 {
        return None;
    }
    let (h, mi, se): (i64, i64, i64) = (
        t[0].parse().ok()?,
        t[1].parse().ok()?,
        t.get(2)
            .map(|x| x.split('.').next().unwrap_or("0").parse().ok())
            .unwrap_or(Some(0))?,
    );

    let days = days_from_civil(y, mo, da) as i64;
    let local = days * 86400 + h * 3600 + mi * 60 + se;
    let utc = local - offset_secs;
    if utc < 0 { None } else { Some(utc as u64) }
}

fn days_from_civil(y: i64, m: i64, d: i64) -> u64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146097 + doe - 719468) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rfc3339_with_offset() {
        let a = parse_rfc3339("2026-12-20T09:00:00+08:00").unwrap();
        let b = parse_rfc3339("2026-12-20T01:00:00Z").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn parse_rfc3339_negative_offset() {
        let a = parse_rfc3339("2026-12-20T01:00:00-05:00").unwrap();
        let b = parse_rfc3339("2026-12-20T06:00:00Z").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn parse_rfc3339_known_epoch() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("1994-11-06T08:49:37Z"), Some(784111777));
    }

    #[test]
    fn parse_rfc3339_rejects_garbage() {
        assert!(parse_rfc3339("").is_none());
        assert!(parse_rfc3339("not-a-date").is_none());
    }
}
