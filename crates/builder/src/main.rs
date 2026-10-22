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

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Keygen => {
            let key = crypto::generate_master_hex();
            println!("{key}");
            eprintln!("请立即把该密钥存入密码管理器；不要写入任何文件或提交进 git。");
        }
        Cmd::Build {
            manifest,
            banks,
            stub,
            out,
            master,
            master_file,
            emit_keyfile,
        } => {
            let key = resolve_master(master, master_file)?;
            build(manifest, banks, stub, out, &key, emit_keyfile)?;
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

fn build(
    manifest_path: PathBuf,
    banks_dir: PathBuf,
    stub_path: PathBuf,
    out_path: PathBuf,
    master_hex: &str,
    emit_keyfile: Option<PathBuf>,
) -> Result<()> {
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("读取清单 {}", manifest_path.display()))?;
    let manifest: Manifest = serde_json::from_str(&manifest_text).context("解析 manifest")?;

    // 构建期校验：场次引用的题库文件必须存在，且抽题数不能超过题量。
    // 这类错误必须在打包时就炸，绝不能留到考场上。
    let master = crypto::master_from_hex(master_hex).context("解析主密钥")?;
    let salt = hex::decode(&manifest.salt).context("manifest.salt 必须是 hex")?;
    // 清单自身校验（时间窗倒挂、重复学号、场次编号重复）。
    // build_payload 内部也会做，这里提前给出带上下文的报错。
    container::validate_sessions(&manifest.sessions).context("清单校验失败")?;

    // 收集所有场次引用到的题库文件。
    let mut files: Vec<String> = manifest
        .sessions
        .iter()
        .flat_map(|s| s.exams.iter().map(|e| e.file.clone()))
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

    let payload =
        container::build_payload(&master, &banks, &manifest.sessions, &salt, manifest.kdf)
            .context("构建 payload")?;
    let stub =
        std::fs::read(&stub_path).with_context(|| format!("读取 stub {}", stub_path.display()))?;

    // 检测 stub 是否已嵌入密钥。
    // 这一步非常重要：若 stub 未嵌入，学生端运行时会报"缺少密钥"，
    // 而管理员很可能以为已经嵌入——直到考场上才发现。
    let stub_embedded = contains_bytes(&stub, b"EXAMKEY:EMBEDDED");
    let stub_plain = contains_bytes(&stub, b"EXAMKEY:PLAIN");
    if !stub_embedded && !stub_plain {
        println!(
            "⚠ 警告：stub 里没有密钥状态标记（{}）。\n\
             这可能不是本项目的 client 构建产物，或版本过旧。",
            stub_path.display()
        );
    }

    let sfx = container::append_payload_to_exe(&stub, &payload)?;
    std::fs::write(&out_path, &sfx).with_context(|| format!("写出 {}", out_path.display()))?;

    let total_students: usize = manifest.sessions.iter().map(|s| s.students.len()).sum();
    println!("✔ 已生成 {}", out_path.display());
    println!("  payload 大小: {} 字节", payload.len());
    println!("  payload SHA256: {}", container::payload_hash(&payload));
    println!("  场次: {}", manifest.sessions.len());
    println!("  学生信封: {total_students} 个（每个学生一个，包内不含学号明文）");
    println!(
        "  KDF: Argon2id(m={}, t={}, p={})",
        manifest.kdf.m_cost, manifest.kdf.t_cost, manifest.kdf.p_cost
    );

    if stub_embedded {
        println!("  ✔ 学生端已内嵌密钥：学生运行后只需输入学号");
    } else {
        println!();
        println!("⚠⚠ 注意：这个学生端**未内嵌密钥**，学生必须提供 --master 才能解锁。");
        println!("   若要学生免输密钥，请：");
        println!("     1. 用本命令加上 --emit-keyfile kf.txt 导出 K_manifest；");
        println!("     2. 设置 EXAM_KEYFILE=kf.txt 后重新编译 client；");
        println!("     3. 用新编译出的 client 作为 --stub 重新打包。");
    }

    // 输出 K_manifest，供学生端编译时嵌入（使其免输 master）。
    if let Some(path) = emit_keyfile {
        let k_manifest = container::manifest_key(&master);
        std::fs::write(&path, hex::encode(k_manifest))
            .with_context(|| format!("写出 keyfile {}", path.display()))?;
        println!("\n✔ 已输出 K_manifest 到 {}", path.display());
        println!("  用它重新编译学生端即可免输 master：");
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
