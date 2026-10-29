//! 学生端构建脚本：把 K_manifest 分片混淆后编译进二进制。
//!
//! ## 为什么只嵌 K_manifest
//!
//! master 在容器格式里**只用于派生 K_manifest**（解 header 用）。
//! 题库和信封都用每场独立的 session_secret 加密，与 master 无关。
//!
//! 因此嵌入 K_manifest 就能让学生"免输 master"，而泄漏后果可控：
//! - 逆向出 K_manifest → 能读场次时间/人数；有合法学号可解本场题干（答案/judge 已剥离），不能读其他场次或标准答案；
//! - 要拿题目，仍需知道学号（触发 Argon2id，约 92ms/次）。
//!
//! 相比之下，现状是"管理员把 master 交给每个学生"，危害大得多。
//!
//! ## 分片混淆不是密码学保护
//!
//! 它只把明文密钥从二进制里消除，让 `strings`/`grep` 搜不到。
//! 有逆向能力的人仍可还原——这一点必须在文档里如实声明。
//!
//! ## 环境变量
//!
//! - `EXAM_KEYFILE`：指向一个含 64 位 hex 的文件（K_manifest 的值）。
//!   由 `exam-builder build --emit-keyfile <path>` 生成。
//!   未设置时生成全零占位，`EMBEDDED = false`，运行时回退到 `EXAM_MASTER` / `--master-file`。

use std::env;
use std::fs;
use std::path::PathBuf;

/// 分片掩码。与索引混合，避免每字节用同一常量。
///
/// 这里用编译期常量，运行时用同样的公式还原。
const MASK: [u8; 32] = [
    0x5a, 0xc3, 0x1f, 0x87, 0x2d, 0xe9, 0x46, 0xb1, 0x73, 0x0c, 0x9e, 0x28, 0xd4, 0x6f, 0x91, 0x3b,
    0xa7, 0x52, 0xce, 0x19, 0x84, 0xfd, 0x60, 0x2e, 0xb8, 0x47, 0xda, 0x05, 0x96, 0x6c, 0x31, 0xef,
];

fn main() {
    println!("cargo:rerun-if-env-changed=EXAM_KEYFILE");
    println!("cargo:rerun-if-changed=build.rs");

    // ⚠ cargo 的已知限制：`rerun-if-env-changed` 只在**值变化**时触发。
    // 若上一次构建时该变量根本没设置过，设置后 cargo 仍可能认为
    // build.rs 无需重跑（实测确实如此），导致"改了密钥却没生效"。
    // 因此额外声明依赖 keyfile 文件本身：文件内容变了就重跑。
    if let Ok(path) = env::var("EXAM_KEYFILE") {
        println!("cargo:rerun-if-changed={path}");
        if !std::path::Path::new(&path).exists() {
            println!("cargo:warning=EXAM_KEYFILE 指向的文件不存在：{path}（将以开发模式构建）");
        }
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR 由 cargo 提供"));
    let target = out_dir.join("embedded_key.rs");

    let key = read_key();
    let embedded = key.is_some();
    let raw = key.unwrap_or([0u8; 32]);

    // 分片：shard[i] = key[i] ^ MASK[i]
    let shards: Vec<String> = raw
        .iter()
        .zip(MASK.iter())
        .map(|(k, m)| format!("0x{:02x}", k ^ m))
        .collect();

    let code = format!(
        "// 由 build.rs 自动生成，请勿手动编辑。\n\
         pub const SHARDS: [u8; 32] = [{}];\n\
         pub const EMBEDDED: bool = {};\n",
        shards.join(", "),
        embedded
    );
    fs::write(&target, code).expect("写出 embedded_key.rs");

    if embedded {
        println!("cargo:warning=已嵌入 K_manifest（学生端无需额外密钥）");
    } else {
        println!(
            "cargo:warning=未嵌入 K_manifest（EXAM_KEYFILE 未设置），\
             学生端需用 EXAM_MASTER 或 --master-file 运行"
        );
    }
}

/// 读取 `EXAM_KEYFILE` 指向的 64 位 hex 密钥。
///
/// 任何一步失败都返回 `None`（走开发模式），**不 panic**——
/// 构建脚本 panic 会让整个编译失败，而"没设环境变量"是完全合法的场景。
fn read_key() -> Option<[u8; 32]> {
    let path = env::var("EXAM_KEYFILE").ok()?;
    let text = fs::read_to_string(&path).ok()?;
    let trimmed = text.trim();
    let bytes = hex_decode(trimmed)?;
    if bytes.len() != 32 {
        println!(
            "cargo:warning=EXAM_KEYFILE 内容长度错误：期望 64 个 hex 字符，实际 {}",
            trimmed.len()
        );
        return None;
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Some(key)
}

/// 最小 hex 解码（构建脚本里不想为此引入依赖）。
fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in bytes.chunks(2) {
        let hi = hex_val(pair[0])?;
        let lo = hex_val(pair[1])?;
        out.push(hi << 4 | lo);
    }
    Some(out)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
