//! exam-client：学生端（合并版单 exe）。
//!
//! 流程（严格按此顺序，门禁必须在派生密钥之前）：
//!
//! ```text
//! 读自身 payload → 解 header（K_manifest）
//!   → 【时间门禁】多源取时 → decide_session 判定场次
//!   → Argon2id(学号, 场次, salt) → 按 tag 定位信封 → 解出会话密钥
//!   → 解密题库 → 确定性抽题 → 展示试卷
//! ```
//!
//! ## 安全约束
//!
//! - `--session` **不能**绕过时间窗：它只能在门禁通过后缩小范围。
//! - `--skip-timegate` 只在 debug 构建里存在，release 编译时被 `cfg` 移除。

mod keys;

use anyhow::{Context, Result};
use clap::Parser;

use common::container::{self, Header};
use common::crypto;
use common::picker;
use common::timegate::{self, HttpsDateSource, SessionWindow, TimeSource, decide_session};

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
    /// 在门禁通过后，进一步指定场次（**不能**绕过时间窗）
    #[arg(long)]
    session: Option<u32>,
    /// 跳过时间门禁。**仅 debug 构建可用**，release 里不存在这个参数。
    #[cfg(debug_assertions)]
    #[arg(long, hide = true)]
    skip_timegate: bool,
}

/// 取解 header 用的 K_manifest。
///
/// 优先级（**嵌入优先**，发布版就是要学生零配置）：
///
/// 1. 编译期嵌入的 K_manifest（由 `EXAM_KEYFILE` 提供，见 build.rs）；
/// 2. `--master-file` / `--master` / 环境变量 `EXAM_MASTER`（开发调试用）。
///
/// 注意：第 2 条拿到的是 **master**，需派生一次 K_manifest；
/// 第 1 条已经是 K_manifest 本身，**不能再派生**。
fn resolve_manifest_key(master: Option<String>, master_file: Option<String>) -> Result<[u8; 32]> {
    if let Some(k) = keys::embedded_manifest_key() {
        // 若同时显式传了 master，校验一致性，避免"以为用了 A 实际用了 B"。
        if master.is_some() || master_file.is_some() {
            let given = read_master_arg(master.as_deref(), master_file.as_deref())?;
            let derived = container::manifest_key(&given);
            anyhow::ensure!(
                derived == k,
                "显式传入的 master 与包内嵌的密钥不匹配（可能拿错了包或密钥）"
            );
        }
        return Ok(k);
    }

    // 回退到显式 master（开发模式）。
    let given = read_master_arg(master.as_deref(), master_file.as_deref())?;
    Ok(container::manifest_key(&given))
}

/// 把参数解析成 master。`master_file` 存在时读文件。
fn read_master_arg(master: Option<&str>, master_file: Option<&str>) -> Result<[u8; 32]> {
    let hex = if let Some(path) = master_file {
        std::fs::read_to_string(path)
            .with_context(|| format!("读取密钥文件 {path}"))?
            .trim()
            .to_string()
    } else if let Some(m) = master {
        m.trim().to_string()
    } else if let Ok(v) = std::env::var("EXAM_MASTER") {
        if v.trim().is_empty() {
            anyhow::bail!("环境变量 EXAM_MASTER 为空");
        }
        v.trim().to_string()
    } else {
        anyhow::bail!(
            "缺少密钥：本包未内嵌密钥，且未提供 master。\n\
             请用 --master-file / --master / 环境变量 EXAM_MASTER 提供。"
        );
    };
    let m = crypto::master_from_hex(&hex).context("解析主密钥")?;
    Ok(*m)
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

    // 2. 解 header。优先用编译期嵌入的 K_manifest，学生无需输入任何密钥。
    let k_manifest = resolve_manifest_key(cli.master.clone(), cli.master_file.clone())?;
    let (header, blob_area) = container::parse_header(payload, &k_manifest)
        .context("解析 header 失败（密钥不匹配？）")?;
    let salt = hex::decode(&header.salt).context("header.salt hex 解码")?;

    // 3. 【时间门禁】必须在派生入场密钥之前。
    let (session_id, net) = run_time_gate(&header, &cli)?;
    let session = header
        .sessions
        .iter()
        .find(|s| s.session_id == session_id)
        .with_context(|| format!("内部错误：门禁返回了不存在的场次 {session_id}"))?;

    // 4. 解锁。
    let unlocked = container::try_unlock_session(session, &header, blob_area, cli.student, &salt)
        .context("解锁失败（包损坏或 KDF 参数异常）")?
        .with_context(|| format!("学号 {} 不在场次 {} 的名单中", cli.student, session_id))?;

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
    // 触碰标记常量，确保它被链接进二进制（builder 依赖它判断 stub 状态）。
    std::hint::black_box(keys::KEY_STATE_MARKER);

    println!("✔ 解锁成功");
    println!("  学号: {}", cli.student);
    if !keys::is_embedded() {
        println!("  模式: 开发（密钥由命令行提供，未嵌入）");
    }
    println!("  场次: {}", unlocked.session_id);
    println!("  时间窗: {} ~ {}", unlocked.start, unlocked.end);
    if let Some(t) = &net {
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
        println!("  ⚠⚠ 已跳过时间门禁（仅 debug 构建可用）⚠⚠");
    }
    println!("  试卷指纹: {}", paper.fingerprint());
    for (exam_id, ids) in &paper.sections {
        println!("  [{exam_id}] 抽中 {} 题: {:?}", ids.len(), ids);
    }
    Ok(())
}

/// 执行时间门禁，返回 (场次编号, 网络时间信息)。
///
/// **任何路径都必须先拿到网络时间**，`--session` 只能在此之后收窄范围。
fn run_time_gate(header: &Header, cli: &Cli) -> Result<(u32, Option<timegate::NetworkTime>)> {
    // 把 header 里的场次转成门禁需要的窗口视图。
    let windows: Vec<SessionWindow> = header
        .sessions
        .iter()
        .filter_map(|s| {
            let start = timegate::parse_rfc3339(&s.start)?;
            let end = timegate::parse_rfc3339(&s.end)?;
            Some(SessionWindow {
                session_id: s.session_id,
                start,
                end,
            })
        })
        .collect();

    #[cfg(debug_assertions)]
    if cli.skip_timegate {
        eprintln!("⚠⚠⚠ 警告：已跳过时间门禁，仅限 debug 调试！⚠⚠⚠");
        // debug 下允许无网络时间，但仍要选出唯一场次。
        let sid = match cli.session {
            Some(id) => id,
            None => windows
                .first()
                .map(|w| w.session_id)
                .context("包内没有场次")?,
        };
        return Ok((sid, None));
    }

    let sources = build_time_sources();
    let net = timegate::fetch_network_time(&sources, 2, 300).map_err(|e| {
        anyhow::anyhow!(
            "无法获取可信的网络时间：{e}\n\
             请检查网络连接。为避免误判，本系统在取时失败时**拒绝解锁**。"
        )
    })?;

    let decision =
        decide_session(&windows, cli.session, net.unix).map_err(|e| anyhow::anyhow!("{e}"))?;
    if decision.overlapped {
        eprintln!(
            "⚠ 注意：有多个场次时间窗重叠，已取 session_id 最小的一场（#{}）。",
            decision.session_id
        );
    }
    Ok((decision.session_id, Some(net)))
}

/// 组装时间源。
fn build_time_sources() -> Vec<Box<dyn TimeSource>> {
    vec![
        Box::new(HttpsDateSource::new("https://www.baidu.com", "baidu")),
        Box::new(HttpsDateSource::new("https://www.aliyun.com", "aliyun")),
        Box::new(HttpsDateSource::new("https://www.tencent.com", "tencent")),
    ]
}
