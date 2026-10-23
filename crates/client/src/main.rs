//! exam-client：学生端（合并版单 exe）。
//!
//! ## 两种运行模式
//!
//! - **无参数启动** → GUI（学生双击 exe，输入学号即可）
//! - **有参数启动** → CLI（CI 验收、脚本、调试）
//!
//! 两种模式共用 [`workflow::unlock`] 同一套主干逻辑，
//! 因此 GUI 与学生实际考试走的是**完全相同的代码路径**，
//! 不存在"GUI 能用但 CLI 挂了"或反过来的情况。

// release 构建不弹控制台窗口；debug 保留控制台便于看日志。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod gui;
mod keys;
mod nav;
mod welcome;
mod workflow;

use anyhow::Result;
use clap::Parser;

use common::timegate;
use workflow::{UnlockOpts, Unlocked};

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

/// 让"下游提前关闭管道"不再 panic。
///
/// 典型场景：`exam-client ... | head -3`，或脚本里 `grep -q` 命中后立即退出。
/// Rust 的 `println!` 在 stdout 写入失败时会 panic，打出
/// `failed printing to stdout: 管道正在被关闭`，但这不是程序错误。
/// 这里把这类 panic 静默转为正常退出（与 `yes | head` 行为一致）。
fn install_broken_pipe_guard() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let text = info.to_string();
        if text.contains("failed printing to stdout") || text.contains("failed printing to stderr")
        {
            std::process::exit(0);
        }
        default(info);
    }));
}

fn main() -> Result<()> {
    install_broken_pipe_guard();
    // 触碰标记常量，确保它被链接进二进制（builder 依赖它判断 stub 状态）。
    std::hint::black_box(keys::KEY_STATE_MARKER);

    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() {
        // GUI 模式：学生双击启动。
        let opts = UnlockOpts::default();
        return gui::run(opts).map_err(|e| anyhow::anyhow!("GUI 启动失败: {e}"));
    }

    // CLI 模式。
    let cli = Cli::parse();
    let opts = UnlockOpts {
        package: cli.package.clone(),
        master: cli.master.clone(),
        master_file: cli.master_file.clone(),
        session: cli.session,
        #[cfg(debug_assertions)]
        skip_time_gate: cli.skip_timegate,
        #[cfg(not(debug_assertions))]
        skip_time_gate: false,
    };
    let unlocked = workflow::unlock(cli.student, &opts)?;
    print_result(&unlocked);
    Ok(())
}

/// CLI 输出。
fn print_result(unlocked: &Unlocked) {
    println!("{}", common::watermark());
    println!();
    println!("✔ 解锁成功");
    println!("  学号: {}", unlocked.student_id);
    println!("  模式: {}", unlocked.key_source.describe());
    println!("  场次: {}", unlocked.session_id);
    println!("  时间窗: {} ~ {}", unlocked.start, unlocked.end);
    if let Some(t) = &unlocked.net {
        println!(
            "  网络时间: {}（{} 个源，分歧 {} 秒）",
            timegate::format_unix_local_zoned(t.unix),
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
    println!("  试卷指纹: {}", unlocked.paper.fingerprint());
    println!();
    for sec in &unlocked.paper.sections {
        println!(
            "【{}】共 {} 题，{} 分",
            sec.title,
            sec.items.len(),
            sec.total_score()
        );
        for (n, item) in sec.items.iter().enumerate() {
            println!("  {}. ({}) {}", n + 1, item.score, item.stem);
            for (j, opt) in item.options.iter().enumerate() {
                println!("       {}. {opt}", workflow::option_label(j));
            }
        }
        println!();
    }
}
