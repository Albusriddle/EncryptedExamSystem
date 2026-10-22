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
        } => {
            let key = resolve_master(master, master_file)?;
            build(manifest, banks, stub, out, &key)?;
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
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("读取题库 {}", path.display()))?;
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
) -> Result<()> {
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("读取清单 {}", manifest_path.display()))?;
    let manifest: Manifest = serde_json::from_str(&manifest_text).context("解析 manifest")?;

    // 构建期校验：场次引用的题库文件必须存在，且抽题数不能超过题量。
    // 这类错误必须在打包时就炸，绝不能留到考场上。
    let master = crypto::master_from_hex(master_hex).context("解析主密钥")?;
    let salt = hex::decode(&manifest.salt).context("manifest.salt 必须是 hex")?;
    if manifest.sessions.is_empty() {
        anyhow::bail!("清单里没有任何场次");
    }

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
    let payload = container::build_payload(&master, &banks, &manifest.sessions, &salt)
        .context("构建 payload")?;
    let stub =
        std::fs::read(&stub_path).with_context(|| format!("读取 stub {}", stub_path.display()))?;
    let sfx = container::append_payload_to_exe(&stub, &payload)?;
    std::fs::write(&out_path, &sfx).with_context(|| format!("写出 {}", out_path.display()))?;

    println!("✔ 已生成 {}", out_path.display());
    println!("  payload 大小: {} 字节", payload.len());
    println!("  payload SHA256: {}", container::payload_hash(&payload));
    println!("  场次: {}", manifest.sessions.len());
    Ok(())
}

fn verify(package: PathBuf, master_hex: &str) -> Result<()> {
    let bytes = std::fs::read(&package).with_context(|| format!("读取 {}", package.display()))?;
    let payload = container::extract_payload(&bytes).context("提取 payload")?;
    let master = crypto::master_from_hex(master_hex)?;

    // K_manifest 用空 salt 派生，所以这里能直接解开 header 做真正的校验。
    let k_manifest = crypto::hkdf_derive(&master, b"", "manifest");
    let (header, data_ct) = container::parse_payload(payload, &k_manifest)
        .context("解 header 失败：主密钥错误或包已损坏")?;
    let salt = hex::decode(&header.salt).context("header.salt 不是合法 hex")?;
    let data = container::decrypt_data(&master, &salt, &data_ct)
        .context("解题库失败：主密钥错误或包已损坏")?;

    println!("✔ 包校验通过");
    println!("  版本: {}", header.version);
    println!("  场次: {}", header.sessions.len());
    for s in &header.sessions {
        println!("    #{} {} ~ {}", s.session_id, s.start, s.end);
    }
    println!("  题库: {}", data.banks.len());
    for b in &data.banks {
        let pick = data.pick_for(&b.exam_id, b.pick);
        println!(
            "    {} 共 {} 题，抽 {} 题{}",
            b.exam_id,
            b.questions.len(),
            pick,
            if b.questions.iter().any(|q| q.answer.is_some()) {
                "  ⚠ 答案未剥离！"
            } else {
                ""
            }
        );
    }
    Ok(())
}
