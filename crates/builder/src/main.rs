//! exam-builder：把题库 + 场次清单打包成学生端自解压 exe。
//!
//! 用法：
//!   exam-builder build --manifest manifest.json --banks banks/ --stub stub.exe --out StudentClient.exe
//!   exam-builder keygen                          # 生成 64-hex 主密钥
//!   exam-builder verify --package StudentClient.exe --master <hex>   # 校验包可解

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use common::container;
use common::crypto;
use common::model::{ExamBank, Manifest};

#[derive(Parser)]
#[command(name = "exam-builder", version, about = "保密考试系统打包器")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 生成 64 位十六进制主密钥
    Keygen,
    /// 构建学生端自解压包
    Build {
        /// 构建清单（管理端导出）
        #[arg(long)]
        manifest: PathBuf,
        /// 题库目录（含若干 *.json）
        #[arg(long)]
        banks: PathBuf,
        /// 学生端 stub（client 编译产物）
        #[arg(long)]
        stub: PathBuf,
        /// 输出文件
        #[arg(long)]
        out: PathBuf,
        /// 允许使用低于生产强度的 KDF/salt（仅用于演示与自动化测试）。
        ///
        /// 默认拒绝：m=8/t=1 的包能正常构建，但枚举 10^5 个学号只需几秒，
        /// 名单保密形同虚设。这个开关存在的唯一目的是让测试不必等 91ms/人。
        #[arg(long)]
        allow_weak_kdf: bool,
        /// 每生 PIN 登录：为每个学生生成 6 位 PIN，信封用 PIN 派生密钥。
        #[arg(long)]
        pin_login: bool,
        /// PIN 导出 CSV（pin_login 时必填，教师发给学生）。
        #[arg(long, requires = "pin_login")]
        emit_pins: Option<PathBuf>,
        /// 允许覆盖已存在的输出文件。
        ///
        /// 默认拒绝：旧实现直接覆盖，教师可能把上一次的包（连同上一场数据）
        /// 静默换掉却不知道。GUI 会在用户确认后自动带上本开关。
        #[arg(long)]
        force: bool,
        /// 64-hex 主密钥。**优先用 --master-file 或环境变量 EXAM_MASTER**，
        /// 因为命令行参数会出现在进程列表里，任何本机用户都能看到。
        #[arg(long, conflicts_with = "master_file")]
        master: Option<String>,
        /// 从文件读取主密钥（文件内容为 64-hex，末尾空白会被忽略）
        #[arg(long)]
        master_file: Option<PathBuf>,
        /// 额外输出 K_manifest 到指定文件（64-hex）。
        /// 该值供编译学生端时通过环境变量 `EXAM_KEYFILE` 嵌入，
        /// 使学生端免输 master。**注意：这是 K_manifest 而非 master**。
        #[arg(long)]
        emit_keyfile: Option<PathBuf>,
        /// **不要**把 K_manifest 写进包（默认会写）。
        ///
        /// 默认写入时，学生双击 exe 即可解锁，无需任何参数。
        /// 加上本开关后，学生必须自行提供 --master——GUI 无法使用，
        /// 仅用于调试或特殊的分发方式。
        #[arg(long)]
        no_embed_key: bool,
    },
    /// 解密并校验学生答题卡（教师用）
    VerifyAnswerSheet {
        /// 答题卡文件（.eea）
        file: PathBuf,
        /// 输出机器可读 JSON（供管理端调用）
        #[arg(long)]
        json: bool,
        /// 学生端包：提供时做交叉校验（试卷指纹与题目必须对得上）
        #[arg(long)]
        package: Option<PathBuf>,
        #[arg(long, conflicts_with = "master_file")]
        master: Option<String>,
        #[arg(long)]
        master_file: Option<PathBuf>,
    },
    /// 考后查看学生原卷（教师用，不过时间门禁）
    ReviewPaper {
        #[arg(long)]
        package: PathBuf,
        #[arg(long)]
        student: u64,
        #[arg(long)]
        session: u32,
        /// 可选：对应答题卡，用于核对题目/显示作答
        #[arg(long)]
        sheet: Option<PathBuf>,
        #[arg(long, conflicts_with = "master_file")]
        master: Option<String>,
        #[arg(long)]
        master_file: Option<PathBuf>,
    },
    /// 从包里导出 PIN 表（需要 master）
    ExportPins {
        #[arg(long)]
        package: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, conflicts_with = "master_file")]
        master: Option<String>,
        #[arg(long)]
        master_file: Option<PathBuf>,
    },
    /// 校验一个包能否被指定密钥解开
    Verify {
        #[arg(long)]
        package: PathBuf,
        #[arg(long, conflicts_with = "master_file")]
        master: Option<String>,
        #[arg(long)]
        master_file: Option<PathBuf>,
    },
}

/// 按优先级取主密钥：--master-file > --master > 环境变量 EXAM_MASTER。
///
/// **显式参数优先于环境变量**。反过来（环境变量优先）会造成极危险的静默错误：
/// 管理员显式传了 A 密钥，但机器上残留的 EXAM_MASTER=B 被悄悄采用，
/// 包用 B 加密而管理员以为用的是 A。
fn resolve_master(master: Option<String>, master_file: Option<PathBuf>) -> Result<String> {
    if let Some(path) = master_file {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("读取密钥文件 {}", path.display()))?;
        return Ok(text.trim().to_string());
    }
    if let Some(m) = master {
        eprintln!(
            "⚠ 警告：主密钥通过命令行传入，本机其他用户可从进程列表读到。\
             建议改用 --master-file 或环境变量 EXAM_MASTER。"
        );
        return Ok(m);
    }
    if let Ok(v) = std::env::var("EXAM_MASTER")
        && !v.trim().is_empty()
    {
        return Ok(v.trim().to_string());
    }
    anyhow::bail!("缺少主密钥：请用 --master-file、--master 或环境变量 EXAM_MASTER 提供")
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

/// 校验答题卡，返回进程退出码：
/// 0 通过 / 2 解密失败 / 3 结构错误 / 4 绑定不匹配 / 5 IO 或参数错误。
fn verify_answer_sheet(
    file: &std::path::Path,
    as_json: bool,
    package: Option<&std::path::Path>,
    master: Option<String>,
    master_file: Option<PathBuf>,
) -> Result<i32> {
    use common::answer_sheet;

    let master_hex = resolve_master(master, master_file)?;
    let master = crypto::master_from_hex(&master_hex).context("解析主密钥")?;
    let k_manifest = container::manifest_key(&master);

    let bytes = match std::fs::read(file) {
        Ok(b) => b,
        Err(e) => {
            emit_result(
                as_json,
                false,
                &format!("读取 {} 失败：{e}", file.display()),
                None,
                None,
            );
            return Ok(5);
        }
    };

    // L1 + L2：解密 + 结构校验（open 内部已做 validate）
    let sheet = match answer_sheet::open(&k_manifest, &bytes) {
        Ok(s) => s,
        Err(e) => {
            let code = match e {
                answer_sheet::AnswerSheetError::Decrypt => 2,
                _ => 3,
            };
            emit_result(as_json, false, &format!("{e}"), None, None);
            return Ok(code);
        }
    };

    // L3：与包交叉校验
    let mut binding: Option<bool> = None;
    if let Some(pkg) = package {
        match check_binding(pkg, &sheet, &k_manifest, &master) {
            Ok(true) => binding = Some(true),
            Ok(false) => {
                emit_result(
                    as_json,
                    false,
                    "绑定校验失败：试卷指纹或题目列表与包内数据不一致",
                    Some(&sheet),
                    Some(false),
                );
                return Ok(4);
            }
            Err(e) => {
                // 包用不了（不是同一个 master 的包、包里没这个场次/学号）与
                // "指纹不一致"是两回事：前者是工具/环境问题，后者才是真的对不上。
                emit_result(
                    as_json,
                    false,
                    &format!("无法完成绑定校验：{e}"),
                    Some(&sheet),
                    None,
                );
                return Ok(5);
            }
        }
    }

    let summary = format!(
        "答题卡 {}\n  学号 {}\n  场次 {}\n  题目 {}（已答 {}，未答 {}）\n  试卷指纹 {}\n  交卷 {}\n  解密 ✓  结构 ✓{}",
        file.display(),
        sheet.student_id,
        sheet.session_id,
        sheet.qids.len(),
        sheet.answered(),
        sheet.qids.len() - sheet.answered(),
        sheet.paper_fingerprint,
        match sheet.submitted_unix {
            Some(t) => format!("{} 已冻结", common::timegate::format_unix_local_zoned(t)),
            None => "未交卷".to_string(),
        },
        match binding {
            Some(true) => "  绑定 ✓（与包一致）",
            _ => "  绑定 －（未提供 --package）",
        }
    );
    emit_result(as_json, true, &summary, Some(&sheet), binding);
    Ok(0)
}

/// 考后查看原卷：教师工具，明确绕过时间门禁，但必须提供 master。
fn review_paper(
    package: &std::path::Path,
    student_id: u64,
    session_id: u32,
    sheet_path: Option<&std::path::Path>,
    master: Option<String>,
    master_file: Option<PathBuf>,
) -> Result<()> {
    use common::picker;

    let master_hex = resolve_master(master, master_file)?;
    let master = crypto::master_from_hex(&master_hex).context("解析主密钥")?;
    let k_manifest = container::manifest_key(&master);
    let bytes =
        std::fs::read(package).with_context(|| format!("读取包 {} 失败", package.display()))?;
    let payload = container::extract_payload(&bytes).context("包里没有 payload")?;
    let (header, blob) = container::parse_header(payload, &k_manifest)
        .context("解析 header 失败（包与主密钥不匹配？）")?;
    let salt = hex::decode(&header.salt).context("header.salt 不是 hex")?;
    let session = header
        .sessions
        .iter()
        .find(|s| s.session_id == session_id)
        .with_context(|| format!("包内没有场次 {session_id}"))?;
    let pins = container::decrypt_pins(&master, &header).context("解密 PIN 表")?;
    let pin = pins
        .as_ref()
        .and_then(|m| m.get(&(session_id, student_id)))
        .map(String::as_str);
    let unlocked =
        container::try_unlock_session_with_pin(session, &header, blob, student_id, &salt, pin)
            .context("解锁信封失败")?
            .with_context(|| format!("学号 {student_id} 不在场次 {session_id} 的名单中"))?;
    let data = container::decrypt_data(&unlocked.session_secret, &salt, &unlocked.data_ct)
        .context("解密题库失败")?;
    let paper = if unlocked.sections.is_empty() {
        picker::generate_paper_ranged(
            student_id,
            session_id,
            &salt,
            &data.banks,
            |exam_id, fallback| (data.pick_for(exam_id, fallback), data.range_for(exam_id)),
        )
        .context("生成原卷失败")?
    } else if unlocked.sections.iter().any(|s| !s.parts.is_empty()) {
        picker::generate_paper_parts(
            student_id,
            session_id,
            &salt,
            &data.banks,
            &unlocked.sections,
        )
        .context("生成原卷（大题部分）失败")?
    } else {
        picker::generate_paper_sections(
            student_id,
            session_id,
            &salt,
            &data.banks,
            &unlocked.sections,
        )
        .context("生成原卷（大题）失败")?
    };
    if let Some(sheet_path) = sheet_path {
        let sheet_bytes = std::fs::read(sheet_path)
            .with_context(|| format!("读取答题卡 {}", sheet_path.display()))?;
        let sheet = common::answer_sheet::open(&k_manifest, &sheet_bytes)
            .context("解密答题卡失败（master 与答题卡不匹配？）")?;
        let paper_qids: Vec<String> = paper
            .sections
            .iter()
            .flat_map(|sec| sec.items.iter().map(|it| it.id.clone()))
            .collect();
        anyhow::ensure!(
            sheet.student_id == student_id
                && sheet.session_id == session_id
                && sheet.paper_fingerprint == paper.fingerprint()
                && sheet.qids == paper_qids,
            "答题卡与包内原卷不一致"
        );
        eprintln!(
            "答题卡：学号 {} 场次 {} 已答 {}/{}",
            sheet.student_id,
            sheet.session_id,
            sheet.answered(),
            sheet.qids.len()
        );
    }
    println!("{}", serde_json::to_string_pretty(&paper)?);
    Ok(())
}

/// 用包内数据重算这名学生的试卷，核对指纹与题目列表。
///
/// **不经过时间门禁**：教师校验发生在考试结束之后，门禁必然已经关闭。
/// 这段逻辑只存在于 builder（教师工具），绝不能进学生端——否则等于给门禁开后门。
fn check_binding(
    package: &std::path::Path,
    sheet: &common::answer_sheet::AnswerSheet,
    k_manifest: &[u8; 32],
    master: &[u8; 32],
) -> Result<bool> {
    use common::picker;

    let bytes =
        std::fs::read(package).with_context(|| format!("读取包 {} 失败", package.display()))?;
    let payload = container::extract_payload(&bytes).context("包里没有 payload")?;
    let (header, blob) = container::parse_header(payload, k_manifest)
        .context("解析 header 失败（包与主密钥不匹配？）")?;
    let salt = hex::decode(&header.salt).context("header.salt 不是 hex")?;
    let session = header
        .sessions
        .iter()
        .find(|s| s.session_id == sheet.session_id)
        .with_context(|| format!("包内没有场次 {}", sheet.session_id))?;
    let pins = container::decrypt_pins(master, &header).context("解密 PIN 表")?;
    let pin = pins
        .as_ref()
        .and_then(|m| m.get(&(sheet.session_id, sheet.student_id)))
        .map(String::as_str);
    let unlocked = container::try_unlock_session_with_pin(
        session,
        &header,
        blob,
        sheet.student_id,
        &salt,
        pin,
    )
    .context("解锁信封失败")?
    .with_context(|| {
        format!(
            "学号 {} 不在场次 {} 的名单中",
            sheet.student_id, sheet.session_id
        )
    })?;
    let data = container::decrypt_data(&unlocked.session_secret, &salt, &unlocked.data_ct)
        .context("解密题库失败")?;
    // 必须与学生端完全一致：旧包（无 sections）走平均难度；
    // v6 单层大题走 sections；v7 两层大题走 parts。
    // 之前这里固定用 generate_paper_ranged，导致所有 v0.15 大题包
    // 的答题卡都被误判为“指纹不一致”。
    let paper = if unlocked.sections.is_empty() {
        picker::generate_paper_ranged(
            sheet.student_id,
            unlocked.session_id,
            &salt,
            &data.banks,
            |exam_id, fallback| (data.pick_for(exam_id, fallback), data.range_for(exam_id)),
        )
        .context("重新生成试卷失败")?
    } else if unlocked.sections.iter().any(|s| !s.parts.is_empty()) {
        picker::generate_paper_parts(
            sheet.student_id,
            unlocked.session_id,
            &salt,
            &data.banks,
            &unlocked.sections,
        )
        .context("按大题部分重新生成试卷失败")?
    } else {
        picker::generate_paper_sections(
            sheet.student_id,
            unlocked.session_id,
            &salt,
            &data.banks,
            &unlocked.sections,
        )
        .context("按大题重新生成试卷失败")?
    };

    if paper.fingerprint() != sheet.paper_fingerprint {
        return Ok(false);
    }
    let qids: Vec<String> = paper
        .sections
        .iter()
        .flat_map(|s| s.items.iter().map(|i| i.id.clone()))
        .collect();
    Ok(qids == sheet.qids)
}

fn export_pins(
    package: &std::path::Path,
    out: &std::path::Path,
    master: Option<String>,
    master_file: Option<PathBuf>,
) -> Result<()> {
    let master_hex = resolve_master(master, master_file)?;
    let master = crypto::master_from_hex(&master_hex).context("解析主密钥")?;
    let bytes = std::fs::read(package).with_context(|| format!("读取 {}", package.display()))?;
    let payload = container::extract_payload(&bytes).context("包里没有 payload")?;
    let k_manifest = container::manifest_key(&master);
    let (header, _) = container::parse_header(payload, &k_manifest).context("解析 header")?;
    let pins = container::decrypt_pins(&master, &header)?
        .context("这个包没有 PIN 表（旧包或未启用「每生 PIN 登录」）")?;
    let mut rows: Vec<((u32, u64), String)> = pins.into_iter().collect();
    rows.sort_by_key(|((s, t), _)| (*s, *t));
    let mut csv = String::from("\u{feff}场次,学号,PIN\n");
    for ((sid, stu), pin) in rows {
        csv.push_str(&format!("{sid},{stu},{pin}\n"));
    }
    write_private(out, csv.as_bytes())?;
    println!("PIN 表已导出：{}", out.display());
    Ok(())
}

fn emit_result(
    as_json: bool,
    ok: bool,
    message: &str,
    sheet: Option<&common::answer_sheet::AnswerSheet>,
    binding: Option<bool>,
) {
    if as_json {
        let payload = serde_json::json!({
            "ok": ok,
            "message": message,
            // null = 没做绑定校验（未提供 --package），不能当成"校验通过"
            "binding": binding,
            "sheet": sheet,
        });
        println!("{payload}");
    } else if ok {
        println!("{message}");
    } else {
        eprintln!("✗ {message}");
    }
}

fn main() -> Result<()> {
    install_broken_pipe_guard();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Keygen => {
            let key = crypto::generate_master_hex();
            println!("{key}");
            eprintln!("请立即把该密钥存入密码管理器；不要写入任何文件或提交进 git。");
        }
        Cmd::VerifyAnswerSheet {
            file,
            json,
            package,
            master,
            master_file,
        } => {
            let code = verify_answer_sheet(&file, json, package.as_deref(), master, master_file)?;
            std::process::exit(code);
        }
        Cmd::ReviewPaper {
            package,
            student,
            session,
            sheet,
            master,
            master_file,
        } => {
            review_paper(
                &package,
                student,
                session,
                sheet.as_deref(),
                master,
                master_file,
            )?;
        }
        Cmd::Build {
            manifest,
            banks,
            stub,
            out,
            allow_weak_kdf,
            force,
            master,
            master_file,
            emit_keyfile,
            no_embed_key,
            pin_login,
            emit_pins,
        } => {
            let key = resolve_master(master, master_file)?;
            build(BuildRequest {
                manifest_path: manifest,
                banks_dir: banks,
                stub_path: stub,
                out_path: out,
                allow_weak_kdf,
                force,
                master_hex: key,
                emit_keyfile,
                embed_key: !no_embed_key,
                pin_login,
                emit_pins,
            })?;
        }
        Cmd::ExportPins {
            package,
            out,
            master,
            master_file,
        } => {
            export_pins(&package, &out, master, master_file)?;
        }
        Cmd::Verify {
            package,
            master,
            master_file,
        } => {
            let key = resolve_master(master, master_file)?;
            verify(package, &key)?;
        }
    }
    Ok(())
}

fn load_banks(dir: &std::path::Path, files: &[String]) -> Result<Vec<ExamBank>> {
    let mut out = Vec::new();
    for f in files {
        let path = dir.join(f);
        let bytes = std::fs::read(&path).with_context(|| {
            format!(
                "读取题库 {} 失败（请确认文件存在，且清单里的 file 是相对 --banks 目录的路径）",
                path.display()
            )
        })?;
        // 单独处理编码错误：`read_to_string` 只会报 "stream did not contain
        // valid UTF-8"，管理员看到会一头雾水。中文题库最常见的坑就是用
        // 记事本存成了 GBK。
        let text = String::from_utf8(bytes).map_err(|_| {
            anyhow::anyhow!(
                "题库 {} 不是 UTF-8 编码。\n\
                 请用支持 UTF-8 的编辑器另存（Windows 记事本选「UTF-8」；\n\
                 VS Code 右下角点击编码 → Save with Encoding → UTF-8）。",
                path.display()
            )
        })?;
        let bank: ExamBank =
            serde_json::from_str(&text).with_context(|| format!("解析题库 {}", path.display()))?;
        bank.validate()
            .map_err(|e| anyhow::anyhow!("题库 {f} 校验失败: {e}"))?;
        out.push(bank);
    }
    Ok(out)
}

/// `build()` 的输入。参数超过 7 个时打包成结构体，避免一长串位置参数。
struct BuildRequest {
    manifest_path: PathBuf,
    banks_dir: PathBuf,
    stub_path: PathBuf,
    out_path: PathBuf,
    allow_weak_kdf: bool,
    force: bool,
    master_hex: String,
    emit_keyfile: Option<PathBuf>,
    embed_key: bool,
    pin_login: bool,
    emit_pins: Option<PathBuf>,
}

fn write_private(path: &std::path::Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .with_context(|| format!("写入 {}", path.display()))?;
    f.write_all(data)
        .with_context(|| format!("写入 {}", path.display()))?;
    Ok(())
}

fn build(req: BuildRequest) -> Result<()> {
    let BuildRequest {
        manifest_path,
        banks_dir,
        stub_path,
        out_path,
        allow_weak_kdf,
        force,
        master_hex,
        emit_keyfile,
        embed_key,
        pin_login,
        emit_pins,
    } = req;

    // 输出/输入路径冲突保护：否则 `--emit-pins` 可以覆盖 exe，或 `--out`
    // 覆盖 manifest/stub，导致“打包成功但文件被 CSV/密钥覆盖”。
    let abs = |p: &std::path::Path| -> std::path::PathBuf {
        p.canonicalize().unwrap_or_else(|_| {
            std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .join(p)
        })
    };
    let out_abs = abs(&out_path);
    let mut protected: Vec<(&str, &std::path::Path)> = vec![
        ("manifest", manifest_path.as_path()),
        ("stub", stub_path.as_path()),
    ];
    if let Some(p) = emit_pins.as_deref() {
        protected.push(("emit-pins", p));
    }
    if let Some(p) = emit_keyfile.as_deref() {
        protected.push(("emit-keyfile", p));
    }
    for (label, p) in protected {
        anyhow::ensure!(
            out_abs != abs(p),
            "输出路径不能与 {label} 相同：{}",
            out_path.display()
        );
    }
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("读取清单 {}", manifest_path.display()))?;
    let manifest: Manifest = serde_json::from_str(&manifest_text).context("解析 manifest")?;

    // 构建期校验：场次引用的题库文件必须存在，且抽题数不能超过题量。
    // 这类错误必须在打包时就炸，绝不能留到考场上。
    let master = crypto::master_from_hex(&master_hex).context("解析主密钥")?;
    let salt = hex::decode(&manifest.salt).context("manifest.salt 必须是 hex")?;

    // 生产强度校验：管理端有同样的下限，但 `--manifest` 是文档化的官方
    // 用法，手工写清单会完全绕过 Python 校验。弱参数的包能被正常构建，
    // 但名单可在秒级被枚举完——必须在打包这一步拦住。
    let kdf = manifest.kdf;
    let weak =
        !kdf.meets_production_strength() || salt.len() < crypto::KdfParams::PROD_MIN_SALT_LEN;
    if weak {
        let detail = format!(
            "KDF: Argon2id(m={}, t={}, p={})，salt {} 字节",
            kdf.m_cost,
            kdf.t_cost,
            kdf.p_cost,
            salt.len()
        );
        if allow_weak_kdf {
            println!("⚠ 警告：使用弱 KDF/salt 打包（{detail}）——仅限演示，切勿分发。");
        } else {
            anyhow::bail!(
                "KDF/salt 低于生产强度，拒绝打包：{detail}\n\
                 要求 m_cost ≥ {}、t_cost ≥ {}、salt ≥ {} 字节。\n\
                 弱参数会让名单在几秒内被枚举完，等于放弃名单保密。\n\
                 仅用于演示/测试时，加 --allow-weak-kdf 显式放行。",
                crypto::KdfParams::PROD_MIN_M_COST,
                crypto::KdfParams::PROD_MIN_T_COST,
                crypto::KdfParams::PROD_MIN_SALT_LEN
            );
        }
    }
    // 清单自身校验（时间窗倒挂、重复学号、场次编号重复）。
    // build_payload 内部也会做，这里提前给出带上下文的报错。
    container::validate_sessions(&manifest.sessions).context("清单校验失败")?;

    // 收集所有场次引用到的题库文件。
    let mut files: Vec<String> = manifest
        .sessions
        .iter()
        .flat_map(|s| {
            s.exams
                .iter()
                .map(|e| e.file.clone())
                .chain(s.sections.iter().map(|sec| sec.file.clone()))
        })
        .collect();
    files.sort();
    files.dedup();
    let banks = load_banks(&banks_dir, &files)?;

    // 构建期硬校验：清单引用的题库必须存在，且抽题数不能超过题量。
    // build_payload 内部也会校验，这里提前给出更友好的报错（带文件名）。
    for s in &manifest.sessions {
        for e in &s.exams {
            let bank = container::match_bank(&e.file, &banks).ok_or_else(|| {
                anyhow::anyhow!(
                    "场次 {} 引用的题库 {} 匹配不到（可用 exam_id: {}）",
                    s.session_id,
                    e.file,
                    banks
                        .iter()
                        .map(|b| b.exam_id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
            let pick = e.pick.unwrap_or(bank.pick);
            anyhow::ensure!(
                pick as usize <= bank.questions.len(),
                "场次 {} 的题库 {} 要求抽 {pick} 题，但题库只有 {} 题",
                s.session_id,
                e.file,
                bank.questions.len()
            );
        }
        let mut section_need: std::collections::HashMap<(String, u8), u32> =
            std::collections::HashMap::new();
        let mut bank_need: std::collections::HashMap<String, u32> =
            std::collections::HashMap::new();
        for sec in &s.sections {
            let bank = container::match_bank(&sec.file, &banks).ok_or_else(|| {
                anyhow::anyhow!(
                    "场次 {} 的大题 {} 引用的题库 {} 匹配不到",
                    s.session_id,
                    sec.title,
                    sec.file
                )
            })?;
            for part in sec.effective_parts() {
                let available = bank
                    .questions
                    .iter()
                    .filter(|q| q.difficulty == Some(part.difficulty))
                    .count();
                let key = (bank.exam_id.clone(), part.difficulty);
                let need = section_need.entry(key).or_insert(0);
                *need += part.count;
                let bank_total = bank_need.entry(bank.exam_id.clone()).or_insert(0);
                *bank_total += part.count;
                anyhow::ensure!(
                    (*need as usize) <= available,
                    "场次 {} 的题库 {} 难度 {} 多个部分共需 {} 题，只有 {} 题",
                    s.session_id,
                    bank.exam_id,
                    part.difficulty,
                    need,
                    available
                );
                anyhow::ensure!(
                    (*bank_total as usize) <= bank.questions.len(),
                    "场次 {} 的题库 {} 多个部分共需 {} 题，题库只有 {} 题",
                    s.session_id,
                    bank.exam_id,
                    bank_total,
                    bank.questions.len()
                );
            }
        }
    }

    // 注意：不再在这里改写 bank.pick。清单的 pick 覆盖由 build_payload
    // 解析成 picks 表写入 data 段，客户端抽题时读取该表。
    // 之前在这里改写 bank.pick 的做法是错的：exam_id 与文件名对不上时会静默失效。
    //
    // 先报个预计耗时：每个学生每场都要跑一次 Argon2id，
    // 大规模清单（如 50 场 × 1000 人 = 5 万个信封）可能要好几分钟，
    // 没有提示的话管理员会以为程序卡死。
    let total_envelopes: usize = manifest.sessions.iter().map(|s| s.students.len()).sum();
    if total_envelopes > 0 {
        // 不估算总时长：Argon2 的实际耗时受内存带宽影响很大，
        // 估算值经常与实际差数倍，给错误数字比不给更糟。
        // 只报"单价 × 数量"，管理员自己就能判断量级。
        println!(
            "⏳ 开始打包：{} 个场次，{} 个学生信封，Argon2id(m={}KiB, t={}, p={})，并行生成",
            manifest.sessions.len(),
            total_envelopes,
            manifest.kdf.m_cost,
            manifest.kdf.t_cost,
            manifest.kdf.p_cost
        );
        println!("   参考：m=65536/t=3 时单次约 91ms；缓存命中前请耐心等待，不要中断。");
    }

    let pins = if pin_login {
        use rand::Rng;
        let mut map = std::collections::HashMap::new();
        for s in &manifest.sessions {
            for &sid in &s.students {
                // 100000–999999：始终 6 位且无前导 0，避免 Excel/CSV 把 028879 显示成 28879。
                let pin = format!("{:06}", rand::rng().random_range(100_000..1_000_000u32));
                map.insert((s.session_id, sid), pin.clone());
            }
        }
        if emit_pins.is_none() {
            anyhow::bail!("--pin-login 必须配合 --emit-pins 导出 PIN 表");
        }
        Some(map)
    } else {
        None
    };

    manifest.time.validate().map_err(anyhow::Error::msg)?;
    let payload = container::build_payload_with_options(
        &master,
        &banks,
        &manifest.sessions,
        &salt,
        manifest.kdf,
        embed_key,
        pins.as_ref(),
        Some(&manifest.time),
        manifest.student_review,
    )
    .context("构建 payload")?;
    let stub =
        std::fs::read(&stub_path).with_context(|| format!("读取 stub {}", stub_path.display()))?;

    // 检测 stub 是不是本项目的 client 产物。
    //
    // 注意：v3 起密钥默认写在**包**（payload 的密钥区）里，不再依赖
    // stub 是否编译期内嵌。所以这里只做"是不是我们的 client"的识别，
    // 不再据此警告"学生需要 --master"——那取决于 embed_key 开关。
    let stub_plain =
        contains_bytes(&stub, b"EXAMKEY:PLAIN") || contains_bytes(&stub, b"EXAMKEY:EMBEDDED");
    if !stub_plain {
        println!(
            "⚠ 警告：stub 里没有密钥状态标记（{}）。\n\
             这可能不是本项目的 client 构建产物，或版本过旧。",
            stub_path.display()
        );
    }

    let sfx = container::append_payload_to_exe(&stub, &payload)?;

    // 覆盖保护：默认不覆盖，避免"上一次的包被静默换掉"。
    if out_path.exists() && !force {
        anyhow::bail!(
            "输出文件已存在：{}\n\
             加 --force 覆盖，或换一个文件名。\n\
             （直接覆盖会让人分不清手里的是哪一次打包的产物。）",
            out_path.display()
        );
    }

    // 原子写：先写同目录的 .tmp，再 rename 覆盖目标。
    // 旧实现直接 fs::write，中途被打断（Ctrl-C / 磁盘满）会留下一个
    // 看起来正常、其实是截断的 exe —— 教师很可能直接发给学生。
    let tmp_path = out_path.with_file_name(format!(
        "{}.tmp",
        out_path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "out.exe".into())
    ));
    let write_result = (|| -> Result<()> {
        std::fs::write(&tmp_path, &sfx)
            .with_context(|| format!("写出临时文件 {}", tmp_path.display()))?;

        // Unix 上必须补执行位：新建文件是 0644，学生端直接运行会
        // "Permission denied"（CI 实测退出码 126）。Windows 无此概念。
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = std::fs::metadata(&tmp_path)?.permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&tmp_path, perm)?;
        }

        // Windows 上 rename 到已存在的文件会失败，先删掉目标。
        if out_path.exists() {
            std::fs::remove_file(&out_path).with_context(|| {
                format!(
                    "无法替换输出文件 {}（可能正在运行、被杀毒软件/OneDrive 占用或只读）。\
                     请关闭正在运行的学生端，或另存为新文件名。",
                    out_path.display()
                )
            })?;
        }
        std::fs::rename(&tmp_path, &out_path)
            .with_context(|| format!("移动到 {}", out_path.display()))?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    write_result?;

    if let (Some(path), Some(map)) = (&emit_pins, &pins) {
        let mut rows: Vec<((u32, u64), String)> =
            map.iter().map(|(k, v)| (*k, v.clone())).collect();
        rows.sort_by_key(|((s, t), _)| (*s, *t));
        let mut csv = String::from("\u{feff}场次,学号,PIN\n");
        for ((sid, stu), pin) in rows {
            csv.push_str(&format!("{sid},{stu},{pin}\n"));
        }
        write_private(path, csv.as_bytes())?;
        println!("PIN 表已导出：{}", path.display());
    }

    let total_students: usize = manifest.sessions.iter().map(|s| s.students.len()).sum();
    println!("✔ 已生成 {}", out_path.display());
    println!("  payload 大小: {} 字节", payload.len());
    println!("  payload SHA256: {}", container::payload_hash(&payload));
    println!("  场次: {}", manifest.sessions.len());
    if embed_key {
        println!("  ✔ 已内嵌密钥：学生双击 exe 输入学号即可，无需任何参数或密钥文件。");
    } else {
        println!(
            "  ⚠ 未内嵌密钥（--no-embed-key）：学生必须自行提供 --master。\n\
             这意味着图形界面无法使用，请确认这是你想要的分发方式。"
        );
    }
    println!("  学生信封: {total_students} 个（每个学生一个，包内不含学号明文）");
    println!(
        "  KDF: Argon2id(m={}, t={}, p={})",
        manifest.kdf.m_cost, manifest.kdf.t_cost, manifest.kdf.p_cost
    );

    // 密钥状态在 payload 组装时已打印，这里只补一句分发提示。
    if embed_key {
        println!("  学生分发：把本文件发给学生，双击 → 输入学号 → 考试。");
    }

    // 输出 K_manifest，供学生端编译时嵌入（使其免输 master）。
    if let Some(path) = emit_keyfile {
        let k_manifest = container::manifest_key(&master);
        write_private(&path, hex::encode(k_manifest).as_bytes())?;
        // Unix 上收窄权限：默认 0644 会让同机其他用户读到 K_manifest。
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&path) {
                let mut perm = meta.permissions();
                perm.set_mode(0o600);
                let _ = std::fs::set_permissions(&path, perm);
            }
        }
        println!("\n✔ 已输出 K_manifest 到 {}", path.display());
        println!("  （可选）用它重新编译学生端，可让密钥编译进 binary 而非包内：");
        println!("    $env:EXAM_KEYFILE = \"{}\"", path.display());
        println!("    cargo build --release -p client");
        println!("  ⚠ 这是 K_manifest 而非 master：泄漏后只能看到场次安排，拿不到题目。");
    }
    Ok(())
}

/// 在字节流里查找子串（stub 里嵌入了状态标记，用于判断是否已内嵌密钥）。
fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn verify(package: PathBuf, master_hex: &str) -> Result<()> {
    let bytes = std::fs::read(&package).with_context(|| format!("读取 {}", package.display()))?;
    let payload = container::extract_payload(&bytes).context("提取 payload")?;
    let master = crypto::master_from_hex(master_hex)?;

    // K_manifest 用空 salt 派生，所以这里能直接解开 header。
    let k_manifest = crypto::hkdf_derive(&master, b"", "manifest");
    let (header, blob_area) = container::parse_header(payload, &k_manifest)
        .context("解 header 失败：主密钥错误或包已损坏")?;
    let _salt = hex::decode(&header.salt).context("header.salt 不是合法 hex")?;

    println!("✔ 包结构校验通过");
    println!("  版本: {}", header.version);
    println!(
        "  KDF: Argon2id(m={}, t={}, p={})",
        header.kdf.m_cost, header.kdf.t_cost, header.kdf.p_cost
    );
    println!("  场次: {}", header.sessions.len());

    // ⚠ 以下信息属于**管理员视图**：只有持有 master 才能看到。
    // 这恰恰说明了 master 的敏感级别——一旦 master 泄漏，
    // 攻击者就能得到全部场次时间与每场人数（虽然仍拿不到学号）。
    // 因此 master 必须与考试包分开保管，绝不能随包分发。
    let mut total_env = 0usize;
    for s in &header.sessions {
        total_env += s.envelopes.len();
        println!(
            "    #{} {} ~ {}  信封 {} 个",
            s.session_id,
            s.start,
            s.end,
            s.envelopes.len()
        );
        let off = s.data_off as usize;
        let len = s.data_len as usize;
        anyhow::ensure!(
            off.checked_add(len)
                .is_some_and(|end| end <= blob_area.len()),
            "场次 {} 的 data 偏移越界（包损坏）",
            s.session_id
        );
    }

    println!("\n⚠ 以上为管理员视图：场次时间与人数只有持 master 者可读。");
    println!("   包里不含学号明文，信封内容无法在不猜中学号的情况下解开。");
    println!("   总信封数 {total_env} 个（= 各场考生数之和）。");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::answer_sheet::{AnswerSheet, seal, write_atomic};

    fn tmpdir(name: &str) -> PathBuf {
        let dir = std::path::Path::new("target")
            .join("verify-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn master_hex() -> String {
        "11".repeat(32)
    }

    /// 造一个最小可用的包，返回 (包路径, K_manifest)。
    fn make_package(dir: &std::path::Path, master: &str, out_name: &str) -> (PathBuf, [u8; 32]) {
        make_package_with_salt(dir, master, out_name, "00112233445566778899aabbccddeeff")
    }

    fn make_package_with_salt(
        dir: &std::path::Path,
        master: &str,
        out_name: &str,
        salt: &str,
    ) -> (PathBuf, [u8; 32]) {
        let banks = dir.join("banks");
        std::fs::create_dir_all(&banks).unwrap();
        std::fs::write(
            banks.join("m.json"),
            r#"{"exam_id":"m","title":"数学","pick":2,"questions":[
                 {"id":"q1","type":"single","stem":"1+1?","options":["1","2"],"answer":"B","score":2},
                 {"id":"q2","type":"single","stem":"2+2?","options":["3","4"],"answer":"B","score":2}]}"#,
        )
        .unwrap();

        let manifest = dir.join("manifest.json");
        std::fs::write(
            &manifest,
            format!(
                r#"{{"version":2,"salt":"{salt}",
                "kdf":{{"m_cost":8,"t_cost":1,"p_cost":1}},
                "sessions":[{{"session_id":1,"start":"2026-01-01T00:00:00+08:00",
                             "end":"2026-12-31T23:59:59+08:00","students":[2021001],
                             "exams":[{{"file":"m.json","pick":2}}]}}]}}"#
            ),
        )
        .unwrap();

        let stub = dir.join("stub.exe");
        std::fs::write(&stub, b"MZ EXAMKEY:EMBEDDED dummy stub").unwrap();

        let out = dir.join(out_name);
        build(BuildRequest {
            manifest_path: manifest,
            banks_dir: banks,
            stub_path: stub,
            out_path: out.clone(),
            allow_weak_kdf: true,
            force: true,
            master_hex: master.to_string(),
            emit_keyfile: None,
            embed_key: true,
            pin_login: false,
            emit_pins: None,
        })
        .unwrap();

        let master_key = crypto::master_from_hex(master).unwrap();
        let k_manifest = container::manifest_key(&master_key);
        (out, k_manifest)
    }

    /// 用包内数据重新生成学生的试卷，造一张答题卡。
    fn make_sheet(pkg: &std::path::Path, k_manifest: &[u8; 32]) -> AnswerSheet {
        let bytes = std::fs::read(pkg).unwrap();
        let payload = container::extract_payload(&bytes).unwrap();
        let (header, blob) = container::parse_header(payload, k_manifest).unwrap();
        let salt = hex::decode(&header.salt).unwrap();
        let session = &header.sessions[0];
        let unlocked = container::try_unlock_session(session, &header, blob, 2021001, &salt)
            .unwrap()
            .unwrap();
        let data =
            container::decrypt_data(&unlocked.session_secret, &salt, &unlocked.data_ct).unwrap();
        let paper = common::picker::generate_paper(2021001, 1, &salt, &data.banks, |e, f| {
            data.pick_for(e, f)
        })
        .unwrap();
        AnswerSheet::from_paper(&paper, 2021001, 1, 1_700_000_000)
    }

    #[test]
    fn verify_sheet_ok_then_tamper_detected() {
        let dir = tmpdir("ok");
        let (pkg, k_manifest) = make_package(&dir, &master_hex(), "p.exe");
        let mut sheet = make_sheet(&pkg, &k_manifest);
        sheet.set_answer(0, Some("B".into()), 1_700_000_100);
        sheet.freeze(1_700_000_200);

        let file = dir.join("answers-2021001-1.eea");
        write_atomic(&file, &seal(&k_manifest, &sheet).unwrap()).unwrap();

        let code = verify_answer_sheet(&file, true, Some(&pkg), Some(master_hex()), None).unwrap();
        assert_eq!(code, 0, "正常答题卡应通过");

        // 改一个字节：必须解密失败（退出码 2）
        let mut bytes = std::fs::read(&file).unwrap();
        let n = bytes.len();
        bytes[n - 1] ^= 0x01;
        std::fs::write(&file, &bytes).unwrap();
        let code = verify_answer_sheet(&file, true, Some(&pkg), Some(master_hex()), None).unwrap();
        assert_eq!(code, 2, "被改动的文件必须解不开");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn verify_sheet_wrong_package_is_rejected() {
        let dir = tmpdir("wrong-pkg");
        let (pkg_a, k_a) = make_package(&dir, &master_hex(), "a.exe");
        // 另一个 master 的包：指纹/密钥都对不上
        let other_master = "22".repeat(32);
        let (pkg_b, _k_b) = make_package(&dir, &other_master, "b.exe");

        let mut sheet = make_sheet(&pkg_a, &k_a);
        sheet.set_answer(0, Some("A".into()), 1);
        let file = dir.join("answers-2021001-1.eea");
        write_atomic(&file, &seal(&k_a, &sheet).unwrap()).unwrap();

        let code =
            verify_answer_sheet(&file, true, Some(&pkg_b), Some(master_hex()), None).unwrap();
        assert_eq!(
            code, 5,
            "拿错包（不同 master）属于'包用不了'，不是绑定不一致"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn verify_sheet_with_same_master_but_other_paper_is_mismatch() {
        let dir = tmpdir("mismatch");
        let (pkg_a, k_a) = make_package(&dir, &master_hex(), "a.exe");
        // 同一个 master，但换一份 salt：密钥能解开，试卷指纹对不上
        let dir_b = dir.join("b");
        std::fs::create_dir_all(&dir_b).unwrap();
        let (pkg_b, _k_b) = make_package_with_salt(
            &dir_b,
            &master_hex(),
            "b.exe",
            "ffeeddccbbaa99887766554433221100",
        );

        let sheet = make_sheet(&pkg_a, &k_a);
        let file = dir.join("answers-2021001-1.eea");
        write_atomic(&file, &seal(&k_a, &sheet).unwrap()).unwrap();

        let code =
            verify_answer_sheet(&file, true, Some(&pkg_b), Some(master_hex()), None).unwrap();
        assert_eq!(code, 4, "同一 master 下换了一份卷：必须判绑定不一致");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn verify_sheet_without_package_skips_binding() {
        let dir = tmpdir("no-pkg");
        let (pkg, k_manifest) = make_package(&dir, &master_hex(), "p.exe");
        let sheet = make_sheet(&pkg, &k_manifest);
        let file = dir.join("answers-2021001-1.eea");
        write_atomic(&file, &seal(&k_manifest, &sheet).unwrap()).unwrap();
        let code = verify_answer_sheet(&file, true, None, Some(master_hex()), None).unwrap();
        assert_eq!(code, 0, "不给包时只做 L1+L2");
        std::fs::remove_dir_all(&dir).ok();
    }
}
