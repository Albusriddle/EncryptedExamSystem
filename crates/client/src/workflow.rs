//! 解锁主干流程。**GUI 与 CLI 共用**，不依赖任何 UI 代码。
//!
//! 抽出来的目的有三个：
//!
//! 1. GUI 只是 `thread::spawn(move || unlock(...))`，不必重复实现流程；
//! 2. 可以脱离 UI 直接测试（`scripts/verify-embed.sh` 走的就是 CLI 路径）；
//! 3. 顺序被固化在这里，**时间门禁一定在派生入场密钥之前**，不会因为
//!    某个界面的疏忽而跳过。

use anyhow::{Context, Result};
use common::container::{self, Header};
use common::crypto;
use common::picker::{self, Paper};
use common::timegate::{self, HttpsDateSource, SessionWindow, TimeSource, decide_session};

use crate::keys;

/// 解锁参数。
#[derive(Debug, Clone, Default)]
pub struct UnlockOpts {
    /// 自解压包路径；`None` 表示读自身。
    pub package: Option<String>,
    /// 主密钥（开发模式用；发布版应通过 `EXAM_KEYFILE` 嵌入）。
    pub master: Option<String>,
    /// 从文件读取主密钥。
    pub master_file: Option<String>,
    /// 在门禁通过后进一步指定场次（**不能**绕过时间窗）。
    pub session: Option<u32>,
    /// 跳过时间门禁（仅 debug 构建有效，见 `main.rs` 的 cfg 处理）。
    pub skip_time_gate: bool,
}

/// 解锁成功后的全部结果。CLI 打印它，GUI 渲染它。
#[derive(Debug, Clone)]
pub struct Unlocked {
    pub student_id: u64,
    pub session_id: u32,
    /// 场次起止（原始 RFC3339 字符串，便于原样展示）
    pub start: String,
    pub end: String,
    /// 网络时间信息；跳过门禁时为 `None`
    pub net: Option<timegate::NetworkTime>,
    pub paper: Paper,
    /// 是否使用了编译期嵌入的密钥（决定 UI 是否提示"开发模式"）
    pub embedded: bool,
}

/// 完整解锁流程。
///
/// 顺序不可调换：
/// ```text
/// 读 payload → 解 header → 【时间门禁】 → 解锁信封 → 解密题库 → 确定性抽题
/// ```
pub fn unlock(student_id: u64, opts: &UnlockOpts) -> Result<Unlocked> {
    // 1. 读自身 payload。
    let exe_path = match &opts.package {
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
    let k_manifest = resolve_manifest_key(opts.master.clone(), opts.master_file.clone())?;
    let (header, blob_area) = container::parse_header(payload, &k_manifest)
        .context("解析 header 失败（密钥不匹配？）")?;
    let salt = hex::decode(&header.salt).context("header.salt hex 解码")?;

    // 3. 【时间门禁】必须在派生入场密钥之前。
    let (session_id, net) = run_time_gate(&header, opts)?;
    let session = header
        .sessions
        .iter()
        .find(|s| s.session_id == session_id)
        .with_context(|| format!("内部错误：门禁返回了不存在的场次 {session_id}"))?;

    // 4. 解锁：Argon2id(学号, 场次, salt) → tag → 信封。
    let unlocked = container::try_unlock_session(session, &header, blob_area, student_id, &salt)
        .context("解锁失败（包损坏或 KDF 参数异常）")?
        .with_context(|| format!("学号 {student_id} 不在场次 {session_id} 的名单中"))?;

    // 5. 解密题库。
    let data = container::decrypt_data(&unlocked.session_secret, &salt, &unlocked.data_ct)
        .context("解密题库失败")?;

    // 6. 确定性抽题。
    let paper = picker::generate_paper(
        student_id,
        unlocked.session_id,
        &salt,
        &data.banks,
        |exam_id, fallback| data.pick_for(exam_id, fallback),
    )
    .context("抽题失败")?;

    Ok(Unlocked {
        student_id,
        session_id: unlocked.session_id,
        start: unlocked.start.clone(),
        end: unlocked.end.clone(),
        net,
        paper,
        embedded: keys::is_embedded(),
    })
}

/// 执行时间门禁，返回 (场次编号, 网络时间信息)。
fn run_time_gate(
    header: &Header,
    opts: &UnlockOpts,
) -> Result<(u32, Option<timegate::NetworkTime>)> {
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
    if opts.skip_time_gate {
        eprintln!("⚠⚠⚠ 警告：已跳过时间门禁，仅限 debug 调试！⚠⚠⚠");
        let sid = match opts.session {
            Some(id) => id,
            None => windows
                .first()
                .map(|w| w.session_id)
                .context("包内没有场次")?,
        };
        return Ok((sid, None));
    }
    #[cfg(not(debug_assertions))]
    let _ = opts.skip_time_gate; // release 下没有这个开关

    let sources = build_time_sources();
    let net = timegate::fetch_network_time(&sources, 2, 300).map_err(|e| {
        anyhow::anyhow!(
            "无法获取可信的网络时间：{e}\n\
             请检查网络连接。为避免误判，本系统在取时失败时**拒绝解锁**。"
        )
    })?;

    let decision =
        decide_session(&windows, opts.session, net.unix).map_err(|e| anyhow::anyhow!("{e}"))?;
    if decision.overlapped {
        eprintln!(
            "⚠ 注意：有多个场次时间窗重叠，已取 session_id 最小的一场（#{}）。",
            decision.session_id
        );
    }
    Ok((decision.session_id, Some(net)))
}

/// 组装时间源。默认用三个国内大站（只要"能上网"就通）。
fn build_time_sources() -> Vec<Box<dyn TimeSource>> {
    vec![
        Box::new(HttpsDateSource::new("https://www.baidu.com", "baidu")),
        Box::new(HttpsDateSource::new("https://www.aliyun.com", "aliyun")),
        Box::new(HttpsDateSource::new("https://www.tencent.com", "tencent")),
    ]
}

/// 选项序号：A-Z，超过 26 个改用数字。
///
/// 原先用 `(b'A' + j.min(25))`，第 26 个之后全部显示为 Z，
/// 学生无法区分（实测：opt25 到 opt29 都是 `Z.`）。
pub fn option_label(j: usize) -> String {
    if j < 26 {
        ((b'A' + j as u8) as char).to_string()
    } else {
        format!("{}", j + 1)
    }
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
pub fn resolve_manifest_key(
    master: Option<String>,
    master_file: Option<String>,
) -> Result<[u8; 32]> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn option_labels_are_unique_up_to_200() {
        let mut seen = std::collections::HashSet::new();
        for j in 0..200 {
            assert!(seen.insert(option_label(j)), "序号重复: j={j}");
        }
    }

    #[test]
    fn option_labels_a_to_z() {
        assert_eq!(option_label(0), "A");
        assert_eq!(option_label(25), "Z");
        assert_eq!(option_label(26), "27");
        assert_eq!(option_label(29), "30");
    }
}
