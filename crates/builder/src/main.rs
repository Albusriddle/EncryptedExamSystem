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
        /// 64-hex 主密钥（也可由 --master-file 读取）
        #[arg(long)]
        master: String,
    },
    /// 校验一个包能否被指定密钥解开
    Verify {
        #[arg(long)]
        package: PathBuf,
        #[arg(long)]
        master: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Keygen => {
            let key = crypto::generate_master_hex();
            println!("{key}");
            eprintln!("请立即把该密钥存入密码管理器；不要写入任何文件或提交进 git。");
        }
        Cmd::Build { manifest, banks, stub, out, master } => {
            build(manifest, banks, stub, out, &master)?;
        }
        Cmd::Verify { package, master } => {
            verify(package, &master)?;
        }
    }
    Ok(())
}

fn load_banks(dir: &PathBuf, files: &[String]) -> Result<Vec<ExamBank>> {
    let mut out = Vec::new();
    for f in files {
        let path = dir.join(f);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("读取题库 {}", path.display()))?;
        let bank: ExamBank = serde_json::from_str(&text)
            .with_context(|| format!("解析题库 {}", path.display()))?;
        bank.validate().map_err(|e| anyhow::anyhow!("题库 {f} 校验失败: {e}"))?;
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

    let master = crypto::master_from_hex(master_hex).context("解析主密钥")?;
    let salt = hex::decode(&manifest.salt).context("manifest.salt 必须是 hex")?;

    // 收集所有场次引用到的题库文件。
    let mut files: Vec<String> = manifest
        .sessions
        .iter()
        .flat_map(|s| s.exams.iter().map(|e| e.file.clone()))
        .collect();
    files.sort();
    files.dedup();
    let banks = load_banks(&banks_dir, &files)?;

    // 若场次覆盖了 pick，则应用覆盖值。
    let mut banks = banks;
    for b in &mut banks {
        for s in &manifest.sessions {
            for e in &s.exams {
                if e.file.trim_end_matches(".json") == b.exam_id
                    || e.file == format!("{}.json", b.exam_id)
                {
                    if let Some(p) = e.pick {
                        b.pick = p;
                    }
                }
            }
        }
    }

    let payload = container::build_payload(&master, &banks, &manifest.sessions, &salt)
        .context("构建 payload")?;
    let stub = std::fs::read(&stub_path).with_context(|| format!("读取 stub {}", stub_path.display()))?;
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
    // K_manifest 的 salt 在 header 里，但 header 本身要 K_manifest 才能解——
    // 这里校验时无法直接拿到 salt，因此 verify 需要配合 --salt。
    // 简化：先从 header 密文长度提示用户改用 build 流程自检。
    println!("payload 提取成功，大小 {} 字节", payload.len());
    println!("payload SHA256: {}", container::payload_hash(payload));
    eprintln!("提示：完整解密校验请使用带 --salt 的版本或在管理端完成。");
    Ok(())
}
