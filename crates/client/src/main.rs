//! exam-client：学生端（合并版单 exe）。
//!
//! 当前为骨架版：从"自解压 exe"提取内嵌 payload，
//! 校验学号 + 时间（M3 接网络时间门禁），解锁后确定性抽题并输出试卷指纹。
//!
//! 真正的 egui 输入窗与 HTTPS 时间服务在 M3/M4 接入；
//! 本版先用命令行参数驱动，便于 M2 端到端验证"加密-解锁-抽题"主干。

use anyhow::{Context, Result};
use clap::Parser;

use common::container;
use common::crypto;
use common::picker;

#[derive(Parser)]
#[command(name = "exam-client", version, about = "保密考试系统学生端")]
struct Cli {
    /// 学号
    #[arg(long)]
    student: u64,
    /// 场次编号（正式版由时间门禁自动判定，骨架版手动指定便于测试）
    #[arg(long)]
    session: u32,
    /// 主密钥（正式版由 stub 分片现场拼装，骨架版手动传入便于测试）
    #[arg(long)]
    master: String,
    /// 自解压包路径（默认读自身）
    #[arg(long)]
    package: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // 1. 读取自身（或指定包）的内嵌 payload。
    let exe_path = match &cli.package {
        Some(p) => p.clone(),
        None => std::env::current_exe()
            .context("获取自身路径")?
            .to_string_lossy()
            .into_owned(),
    };
    let bytes = std::fs::read(&exe_path).with_context(|| format!("读取 {exe_path}"))?;
    let payload = container::extract_payload(&bytes).context(
        "未找到内嵌 payload。骨架版请用 --package 指向 builder 生成的 exe",
    )?;

    // 2. 用主密钥解 header（正式版主密钥由 stub 分片拼装 + 时间门禁后才给）。
    let master = crypto::master_from_hex(&cli.master).context("解析主密钥")?;

    // header 里带 salt，但解 header 需要 K_manifest，K_manifest 派生需要 salt。
    // 解决：salt 同时以明文形式放在 payload 头部之外的固定位置？——
    // 更简单：K_manifest 派生用空 salt，header 解密后再取真实 salt 派生 K_payload。
    // 但当前 build_payload 用 manifest.salt 派生了 K_manifest。
    // 骨架版折中：暂时用空 salt 解 header 的流程在正式版由 stub 完成；
    // 这里为了打通主干，要求调用方同时提供 salt（正式版不需要）。
    //
    // —— 改为：builder 把 salt 明文放在 payload 前 16 字节之外不可取。
    // 最干净的做法：K_manifest = HKDF(master, salt="", "manifest")，
    // header 内含真实 salt，解出 header 后再派生 K_payload / K_session。
    //
    // 因此这里先用空 salt 派生 K_manifest。
    let k_manifest = crypto::hkdf_derive(&master, b"", "manifest");
    let (header, data_ct) = container::parse_payload(payload, &k_manifest)
        .context("解析 header 失败（密钥错误？）")?;
    let salt = hex::decode(&header.salt).context("header.salt hex 解码")?;

    // 3. 解锁指定场次（校验学号是否在名单）。
    let entry = header
        .sessions
        .iter()
        .find(|e| e.session_id == cli.session)
        .with_context(|| format!("场次 {} 不存在", cli.session))?;
    let unlocked = container::try_unlock_session(entry, &master, &salt, cli.student)
        .context("解锁场次")?
        .with_context(|| format!("学号 {} 不在场次 {} 的名单中", cli.student, cli.session))?;

    // 4. 解密题库。
    let data = container::decrypt_data(&master, &salt, &data_ct).context("解密题库")?;

    // 5. 确定性抽题。
    let paper = picker::generate_paper(cli.student, unlocked.session_id, &salt, &data.banks);

    // 6. 输出结果（骨架版打印指纹；正式版进入考试界面）。
    println!("✔ 解锁成功");
    println!("  学号: {}", cli.student);
    println!("  场次: {}", unlocked.session_id);
    println!("  时间窗: {} ~ {}", unlocked.start, unlocked.end);
    println!("  试卷指纹: {}", paper.fingerprint());
    for (exam_id, ids) in &paper.sections {
        println!("  [{exam_id}] 抽中 {} 题: {:?}", ids.len(), ids);
    }
    Ok(())
}
