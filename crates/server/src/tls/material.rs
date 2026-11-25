//! 证书的生成、加载、元数据与备份。
//!
//! 这里只回答「现有证书还能不能用、不能用就重签一张」；「这套证书到底行不行」
//! 由 `check.rs` 的启动自检回答（见 `tls/mod.rs` 模块文档）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

use crate::secure_fs;

/// 已就绪的 TLS 材料。
///
/// 放在本模块（而不是 `mod.rs`）：只有「加载/生成」会产生它，`check.rs` 完全用不到；
/// `mod.rs` 只负责 `pub use` 出去，保持对外路径 `crate::tls::TlsMaterial` 不变。
pub struct TlsMaterial {
    pub config: Arc<ServerConfig>,
    /// 证书 SHA-256 指纹（大写十六进制、冒号分隔）。
    pub fingerprint: String,
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
    /// 是不是本次启动新生成（或被重签）的。
    pub generated: bool,
    /// 证书覆盖的地址；外部证书（`--tls-cert`）未知时为空。
    pub hosts: Vec<String>,
    /// 证书到期时间（Unix 秒）；外部证书未知时为 0。
    pub not_after_unix: u64,
    /// 私钥权限是否真的收紧成功。
    ///
    /// 收紧失败不再中断启动（FAT32/网络盘上 ACL 接口本来就会失败），但**横幅不能
    /// 无条件写「仅当前用户可读」**——那会与事实相反，老师也不会注意 stderr。
    pub key_hardened: bool,
}

const CERT_FILE: &str = "server.crt";
const KEY_FILE: &str = "server.key";
/// 证书的元数据（到期时间 + SAN）。没有它就不知道现有证书是否还适用。
const META_FILE: &str = "server.json";
/// 到期前这么多天就重签：留出「考试当天才发现过期」的余量。
const RENEW_BEFORE_SECS: u64 = 30 * 24 * 3600;

/// 元数据格式版本：将来加字段/改语义时，旧文件必须被当成「不认识」而不是被
/// `serde` 默认值静默接受（否则行为不可预测）。
const META_VERSION: u32 = 1;

/// 证书元数据。自签证书没什么可保密的，明文存即可。
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct CertMeta {
    /// 旧文件没有这个字段 → `default` 得到 0 → 被判定为「不认识的版本」→ 重签。
    #[serde(default)]
    version: u32,
    not_after_unix: u64,
    hosts: Vec<String>,
}

/// 加载已有证书；不适用（过期/换地址/元数据缺失/损坏）就重签一张。
pub fn load_or_generate(dir: &Path, hosts: &[String], days: i64) -> Result<TlsMaterial> {
    let cert_path = dir.join(CERT_FILE);
    let key_path = dir.join(KEY_FILE);
    let meta_path = dir.join(META_FILE);
    let have_pair = cert_path.is_file() && key_path.is_file();

    // 提前量不能吃掉整个有效期：测试/排障时可能用很短的 days（例如 30 天），
    // 固定「提前 30 天」会让「刚生成就判过期」→ 每次启动都重签。
    let renew_before = RENEW_BEFORE_SECS.min((days.max(1) as u64).saturating_mul(86400) / 2);

    let mut reason: Option<String> = None;
    let meta = read_meta(&meta_path);
    if have_pair {
        match &meta {
            Some(meta) => {
                let renew_at = meta.not_after_unix.saturating_sub(renew_before);
                if meta.version != META_VERSION {
                    // 旧版本（没有 version 字段 → 0）或将来降级：按「不认识」处理。
                    reason = Some(format!(
                        "证书元数据版本 {} 不认识（当前 {META_VERSION}）",
                        meta.version
                    ));
                } else if unix_now() >= renew_at {
                    reason = Some("证书已过期或将在 30 天内到期".to_string());
                } else if let Some(missing) = hosts_missing(&meta.hosts, hosts) {
                    reason = Some(format!("本机地址变了：证书 SAN 不含 {missing}"));
                }
            }
            // 老版本目录：只写了证书与私钥、没有元数据 → 无法判断是否过期，直接重签。
            None => reason = Some("证书目录缺少元数据（旧版本生成的）".to_string()),
        }
    }

    let mut generated = false;
    if reason.is_some() || !have_pair {
        if let Some(why) = &reason {
            println!("证书已重签：{why}");
        }
        backup_existing(dir)?;
        generate(dir, hosts, days)?;
        generated = true;
    }

    let material = match load(&cert_path, &key_path) {
        Ok(material) => material,
        // 损坏自愈：文件在但读不出来（截断/换行损坏/私钥不匹配）→ 备份后重签。
        Err(e) if !generated => {
            eprintln!("⚠ 证书/私钥无法加载（{e}）：已备份并重新生成");
            backup_existing(dir)?;
            generate(dir, hosts, days)?;
            generated = true;
            load(&cert_path, &key_path).context("重新生成后仍无法加载证书")?
        }
        Err(e) => return Err(e.context("刚生成的证书无法加载")),
    };

    // 生成/加载成功后元数据必须存在；缺失说明 generate 写盘失败，宁可报错也不要
    // 下次启动又判「元数据缺失」来回重签。
    let meta = read_meta(&meta_path).context("证书元数据缺失（server.json 写入失败？）")?;
    Ok(TlsMaterial {
        generated,
        hosts: meta.hosts,
        not_after_unix: meta.not_after_unix,
        ..material
    })
}

/// 加载指定路径的证书与私钥（老师用自己的 CA 签的证书时走这里）。
///
/// 外部证书的**校验**不在这里做：`load()` 只负责解析与组配置，检查统一走
/// `crate::tls::check_external`（在 `main()` 里、绑定端口之前跑），这样「拒绝启动」发生在
/// 任何监听建立之前，也不会把两种错误混在一条消息里。
/// （这里用反引号而不是 intra-doc link：拆分后它属于 `check.rs`，本模块作用域里没有该名字，
/// `[`check_external`]` 会让 `cargo doc` 报 unresolved link。）
pub fn load(cert_path: &Path, key_path: &Path) -> Result<TlsMaterial> {
    // 加载时也补一次权限收紧：生成时若因文件系统不支持而失败，后续每次启动都再试一遍，
    // 不让「私钥对其他本地用户可读」长期留着。失败只告警，绝不阻断启动。
    let key_hardened = match secure_fs::harden_file(key_path) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("⚠ 私钥权限收紧失败（{e}）：私钥文件可能对其他本地用户可读");
            false
        }
    };

    let (certs, key) = read_material(cert_path, key_path)?;

    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs.clone(), key)
        .context("构建 TLS 配置失败（证书与私钥不匹配？）")?;
    // 显式只声明 http/1.1：服务端不实现 h2，声明清楚比让浏览器猜更好（也不会出现
    // 「浏览器按 h2 发 preface、服务端当 HTTP/1.1 解析」这类怪现象）。
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsMaterial {
        config: Arc::new(config),
        fingerprint: fingerprint_hex(certs[0].as_ref()),
        cert_path: cert_path.to_path_buf(),
        key_path: key_path.to_path_buf(),
        generated: false,
        hosts: Vec::new(),
        not_after_unix: 0,
        key_hardened,
    })
}

/// 读取证书链与私钥（`load()` 与外部证书自检共用）。
pub(super) fn read_material(
    cert_path: &Path,
    key_path: &Path,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let cert_pem =
        std::fs::read(cert_path).with_context(|| format!("读取证书 {}", cert_path.display()))?;
    let key_pem =
        std::fs::read(key_path).with_context(|| format!("读取私钥 {}", key_path.display()))?;
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut &cert_pem[..])
        .collect::<Result<Vec<_>, _>>()
        .context("解析证书 PEM 失败")?;
    anyhow::ensure!(!certs.is_empty(), "证书文件里没有证书");
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut &key_pem[..])
        .context("解析私钥 PEM 失败")?
        .context("私钥文件里没有可用的私钥")?;
    Ok((certs, key))
}

/// 生成一张自签证书（ECDSA P-256），并写下元数据。
///
/// `hosts` 里的字符串由 `rcgen` 自动识别：能解析成 IP 的进 IP SAN，其余进 DNS SAN。
pub fn generate(dir: &Path, hosts: &[String], days: i64) -> Result<()> {
    anyhow::ensure!(!hosts.is_empty(), "生成证书至少要一个 host（IP 或域名）");
    std::fs::create_dir_all(dir).with_context(|| format!("创建证书目录 {}", dir.display()))?;

    // rcgen 0.12：密钥对由 `Certificate::from_params` 内部生成（算法由 params.alg 指定）。
    let mut params = rcgen::CertificateParams::new(hosts.to_vec());
    params.alg = &rcgen::PKCS_ECDSA_P256_SHA256;
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "exam-server");
    params.is_ca = rcgen::IsCa::NoCa;
    // 这两项决定 Chrome 报的是「可绕过」还是「不可绕过」的错误。
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::DigitalSignature,
        rcgen::KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    // 往前留一天，容忍学生设备/教师机的时钟小偏差。
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(days.max(1));

    let cert = rcgen::Certificate::from_params(params).context("签发自签证书失败")?;
    let cert_pem = cert.serialize_pem().context("导出证书 PEM 失败")?;
    let key_pem = cert.serialize_private_key_pem();

    let cert_path = dir.join(CERT_FILE);
    let key_path = dir.join(KEY_FILE);
    let meta_path = dir.join(META_FILE);

    std::fs::write(&cert_path, cert_pem.as_bytes())
        .with_context(|| format!("写入证书 {}", cert_path.display()))?;
    let _ = secure_fs::harden_file(&cert_path);

    // 私钥「先收紧后落盘」：先写 .tmp、在临时名上收紧权限、再 rename 成最终名。
    // 这样最终文件从出现的第一刻就是 0600（Unix）/ 受保护 DACL（Windows），
    // 不存在「写入瞬间人人可读」的窗口。
    let key_tmp = dir.join(format!("{KEY_FILE}.tmp"));
    std::fs::write(&key_tmp, key_pem.as_bytes())
        .with_context(|| format!("写入私钥 {}", key_tmp.display()))?;
    if let Err(e) = secure_fs::harden_file(&key_tmp) {
        // 收紧失败**不阻断启动**：答题卡目录放在 FAT32 U 盘 / 网络共享时 ACL 接口
        // 本来就会失败，为此让整场考试起不来不值得（与状态文件的口径一致：只告警）。
        eprintln!("⚠ 私钥权限收紧失败（{e}）：私钥可能对其他本地用户可读");
    }
    std::fs::rename(&key_tmp, &key_path)
        .with_context(|| format!("落盘私钥 {}", key_path.display()))?;

    let not_after_unix = (now + time::Duration::days(days.max(1)))
        .unix_timestamp()
        .max(0) as u64;
    write_meta(&meta_path, hosts, not_after_unix)?;
    Ok(())
}

/// 把已有证书文件改名成 `.bak.<时间戳>`（重签/自愈前调用）。
///
/// 为什么不直接删：万一重签失败，老师的旧证书还在（虽然自签证书不心疼，
/// 但「删掉又生成失败」会让现场更慌）。
fn backup_existing(dir: &Path) -> Result<()> {
    let stamp = unix_now();
    for name in [CERT_FILE, KEY_FILE, META_FILE] {
        let path = dir.join(name);
        if !path.exists() {
            continue;
        }
        let backup = dir.join(format!("{name}.bak.{stamp}"));
        // rename 失败必须中止：继续往下走 `generate()` 会用 `write` **截断**原文件，
        // 万一只写了一半就失败，现场会同时失去「旧证书」和「新证书」。
        std::fs::rename(&path, &backup).with_context(|| {
            format!(
                "备份旧证书失败（{} 可能被其它程序占用）：{backup:?}",
                path.display()
            )
        })?;
        prune_backups(dir, name, BACKUPS_TO_KEEP);
    }
    Ok(())
}

/// 每个文件最多保留多少份 `.bak.<时间戳>`。
///
/// DHCP 换网段、反复重启都会触发重签；不清理的话 `tls/` 会一直长（每次 3 个文件）。
const BACKUPS_TO_KEEP: usize = 3;

/// 只保留最近 `keep` 份 `<name>.bak.<时间戳>`，按时间戳从新到旧。
fn prune_backups(dir: &Path, name: &str, keep: usize) {
    let prefix = format!("{name}.bak.");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut items: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let file_name = path.file_name()?.to_string_lossy().to_string();
            let stamp = file_name.strip_prefix(&prefix)?.parse::<u64>().ok()?;
            Some((stamp, path))
        })
        .collect();
    items.sort_by_key(|(stamp, _)| std::cmp::Reverse(*stamp));
    for (_, path) in items.into_iter().skip(keep) {
        let _ = std::fs::remove_file(path);
    }
}

/// 目标 SAN 里哪些地址是现有证书没覆盖的（比较前规范化）。
///
/// 为什么要规范化：老师手写 `--tls-host LOCALHOST`、或 IPv6 写成
/// `0:0:0:0:0:0:0:1` 而元数据里是 `localhost` / `::1` 时，严格字符串比较会
/// **每次启动都判「地址变了」→ 重签**，指纹跟着变，学生核对指纹就失效了。
fn hosts_missing(meta_hosts: &[String], want: &[String]) -> Option<String> {
    let have: std::collections::HashSet<String> =
        meta_hosts.iter().map(|h| normalize_host(h)).collect();
    want.iter()
        .find(|h| !have.contains(&normalize_host(h)))
        .cloned()
}

pub(crate) fn normalize_host(host: &str) -> String {
    let trimmed = host.trim();
    match trimmed.parse::<std::net::IpAddr>() {
        // `Ipv6Addr` 的 Display 是规范压缩形式：`0:0:0:0:0:0:0:1` → `::1`
        Ok(ip) => ip.to_string(),
        Err(_) => trimmed.to_ascii_lowercase(),
    }
}

fn read_meta(path: &Path) -> Option<CertMeta> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_meta(path: &Path, hosts: &[String], not_after_unix: u64) -> Result<()> {
    let meta = CertMeta {
        version: META_VERSION,
        not_after_unix,
        hosts: hosts.to_vec(),
    };
    let text = serde_json::to_string_pretty(&meta).context("序列化证书元数据失败")?;
    std::fs::write(path, text).with_context(|| format!("写入证书元数据 {}", path.display()))?;
    Ok(())
}

pub(super) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 证书指纹：SHA-256，大写十六进制、冒号分隔（与浏览器「证书详情」里一致）。
/// 证书 DER 的 SHA-256，格式化成大写十六进制、冒号分隔。
///
/// 只在 tls 模块内用（`load`/`load_or_generate` 组装 `TlsMaterial` 时算一次），
/// 因此保持私有——`mod.rs` 不再把它 `pub use` 出去（拆分后它没有对外调用点）。
fn fingerprint_hex(der: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(der);
    digest
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tls::testutil::*;

    fn hosts() -> Vec<String> {
        vec!["127.0.0.1".to_string(), "192.168.1.10".to_string()]
    }

    /// 生成后能立刻加载，且指纹是 32 组冒号分隔的大写十六进制。
    #[test]
    fn generated_cert_loads_and_has_fingerprint() {
        let dir = test_dir("gen");
        generate(&dir, &hosts(), 30).expect("生成证书");
        let material = load_or_generate(&dir, &hosts(), 30).expect("加载证书");
        assert!(!material.generated, "元数据与地址都没变时应复用");
        let parts: Vec<&str> = material.fingerprint.split(':').collect();
        assert_eq!(parts.len(), 32, "SHA-256 指纹应有 32 组");
        assert!(parts.iter().all(|p| p.len() == 2), "每组应为两字符");
        assert!(
            material
                .fingerprint
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c == ':'),
            "只允许十六进制与冒号：{}",
            material.fingerprint
        );
        assert_eq!(material.hosts, hosts(), "元数据里应记下 SAN 列表");
        assert!(material.not_after_unix > unix_now(), "到期时间应在未来");
        // 临时目录由 `TestDir` 的 Drop 回收（失败也会回收）；这里不再显式删除，
        // 免得同一件事有两套写法。
    }

    /// 证书到期前 30 天内必须重签（文档承诺「到期会自动重签」，就得真的做到）。
    #[test]
    fn expired_meta_triggers_regeneration() {
        let dir = test_dir("expire");
        generate(&dir, &hosts(), 30).expect("生成证书");
        // 把元数据改成「已经过期」
        write_meta(&dir.join(META_FILE), &hosts(), 1).expect("改写元数据");
        let material = load_or_generate(&dir, &hosts(), 30).expect("重签并加载");
        assert!(material.generated, "过期后必须重签");
        assert!(material.not_after_unix > unix_now() + 29 * 86400);
        // 旧证书被备份，不是直接删掉
        let backups: Vec<_> = std::fs::read_dir(&dir)
            .expect("读目录")
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".bak."))
            .collect();
        assert!(!backups.is_empty(), "重签前应备份旧文件");
        // 临时目录由 `TestDir` 的 Drop 回收（失败也会回收）；这里不再显式删除，
        // 免得同一件事有两套写法。
    }

    /// 本机地址变了（SAN 不含新地址）也必须重签。
    #[test]
    fn changed_hosts_trigger_regeneration() {
        let dir = test_dir("hosts");
        generate(&dir, &hosts(), 30).expect("生成证书");
        let mut next = hosts();
        next.push("10.0.0.7".to_string());
        let material = load_or_generate(&dir, &next, 30).expect("重签并加载");
        assert!(material.generated, "出现新地址时必须重签");
        assert!(material.hosts.contains(&"10.0.0.7".to_string()));
        // 临时目录由 `TestDir` 的 Drop 回收（失败也会回收）；这里不再显式删除，
        // 免得同一件事有两套写法。
    }

    /// 老版本目录（只有证书/私钥、没有 server.json）→ 视为未知，重签。
    #[test]
    fn missing_meta_triggers_regeneration() {
        let dir = test_dir("nometa");
        generate(&dir, &hosts(), 30).expect("生成证书");
        std::fs::remove_file(dir.join(META_FILE)).expect("删掉元数据");
        let material = load_or_generate(&dir, &hosts(), 30).expect("重签并加载");
        assert!(material.generated, "缺少元数据时必须重签");
        assert!(dir.join(META_FILE).is_file(), "重签后应重新写下元数据");
        // 临时目录由 `TestDir` 的 Drop 回收（失败也会回收）；这里不再显式删除，
        // 免得同一件事有两套写法。
    }

    /// 证书文件损坏时自愈（备份 + 重签），而不是让服务端起不来。
    #[test]
    fn corrupt_cert_self_heals() {
        let dir = test_dir("corrupt");
        generate(&dir, &hosts(), 30).expect("生成证书");
        std::fs::write(dir.join(CERT_FILE), b"not a pem").expect("写坏证书");
        let material = load_or_generate(&dir, &hosts(), 30).expect("自愈后加载");
        assert!(material.generated, "损坏后必须重签");
        assert!(
            material
                .fingerprint
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c == ':'),
            "重签后指纹应正常"
        );
        // 临时目录由 `TestDir` 的 Drop 回收（失败也会回收）；这里不再显式删除，
        // 免得同一件事有两套写法。
    }

    /// 私钥文件必须只有当前用户可读（Unix 0600；Windows 受保护 DACL）。
    #[test]
    fn private_key_is_hardened() {
        let dir = test_dir("perm");
        generate(&dir, &["127.0.0.1".to_string()], 30).expect("生成证书");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(KEY_FILE))
                .expect("读私钥元数据")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "私钥必须是 0600，实际 {mode:o}");
        }
        #[cfg(windows)]
        {
            assert!(
                secure_fs::acl_grants_only_current_user(&dir.join(KEY_FILE)).unwrap_or(false),
                "私钥 DACL 必须只授予当前用户"
            );
        }
        // 「先收紧后落盘」：临时文件必须已经被 rename 走，不留残件
        assert!(
            !dir.join(format!("{KEY_FILE}.tmp")).exists(),
            "私钥临时文件不应残留"
        );
        // 临时目录由 `TestDir` 的 Drop 回收（失败也会回收）；这里不再显式删除，
        // 免得同一件事有两套写法。
    }

    /// 每个文件只保留最近 3 份备份：DHCP 换网段/反复重启会不断重签，
    /// 不清理的话 `tls/` 会一直长（每次重签 3 个文件）。
    #[test]
    fn backups_are_pruned_to_three() {
        let dir = test_dir("prune");
        std::fs::create_dir_all(&dir).expect("建目录");
        for stamp in 1..=5u64 {
            std::fs::write(dir.join(format!("{CERT_FILE}.bak.{stamp}")), b"x").expect("写备份");
        }
        prune_backups(&dir, CERT_FILE, BACKUPS_TO_KEEP);
        let mut left: Vec<u64> = std::fs::read_dir(&dir)
            .expect("读目录")
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                name.strip_prefix(&format!("{CERT_FILE}.bak."))?
                    .parse::<u64>()
                    .ok()
            })
            .collect();
        left.sort_unstable();
        assert_eq!(left, vec![3, 4, 5], "必须只留最新的三份");
        // 临时目录由 `TestDir` 的 Drop 回收（失败也会回收）；这里不再显式删除，
        // 免得同一件事有两套写法。
    }

    /// SAN 比对要规范化：域名大小写、IPv6 的不同写法都不该触发重签
    /// （否则指纹每次启动都变，学生核对指纹就失效了）。
    #[test]
    fn hosts_missing_normalizes_case_and_ipv6() {
        let meta = vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
            "0:0:0:0:0:0:0:1".to_string(),
        ];
        assert_eq!(hosts_missing(&meta, &["LOCALHOST".to_string()]), None);
        assert_eq!(
            hosts_missing(&meta, &["LocalHost".to_string(), "::1".to_string()]),
            None,
            "IPv6 的完整写法与压缩写法必须视为同一个地址"
        );
        assert_eq!(
            hosts_missing(&meta, &["10.0.0.7".to_string()]),
            Some("10.0.0.7".to_string())
        );
    }

    /// 元数据版本不认识时重签（而不是把未知字段当默认值继续用）。
    #[test]
    fn unknown_meta_version_triggers_regeneration() {
        let dir = test_dir("metaversion");
        generate(&dir, &hosts(), 30).expect("生成证书");
        let text = format!(
            r#"{{"version":99,"not_after_unix":{},"hosts":{}}}"#,
            unix_now() + 86_400,
            serde_json::to_string(&hosts()).expect("序列化 hosts")
        );
        std::fs::write(dir.join(META_FILE), text).expect("写元数据");
        let material = load_or_generate(&dir, &hosts(), 30).expect("重签并加载");
        assert!(material.generated, "版本不认识时必须重签");
        // 临时目录由 `TestDir` 的 Drop 回收（失败也会回收）；这里不再显式删除，
        // 免得同一件事有两套写法。
    }

    /// 老格式（没有 `version` 字段）也必须重签：默认值 0 ≠ 当前版本。
    #[test]
    fn legacy_meta_without_version_triggers_regeneration() {
        let dir = test_dir("legacymeta");
        generate(&dir, &hosts(), 30).expect("生成证书");
        let text = format!(
            r#"{{"not_after_unix":{},"hosts":{}}}"#,
            unix_now() + 86_400,
            serde_json::to_string(&hosts()).expect("序列化 hosts")
        );
        std::fs::write(dir.join(META_FILE), text).expect("写旧格式元数据");
        let material = load_or_generate(&dir, &hosts(), 30).expect("重签并加载");
        assert!(material.generated, "缺 version 的旧元数据必须重签");
        // 临时目录由 `TestDir` 的 Drop 回收（失败也会回收）；这里不再显式删除，
        // 免得同一件事有两套写法。
    }

    /// 证书里必须有 IP SAN（否则 Chrome 报「名称不匹配」，体验更差）。
    #[test]
    fn certificate_contains_ip_san() {
        let dir = test_dir("san");
        generate(&dir, &["192.168.1.10".to_string()], 30).expect("生成证书");
        let pem = std::fs::read(dir.join(CERT_FILE)).expect("读证书");
        let der = rustls_pemfile::certs(&mut &pem[..])
            .next()
            .expect("解析出证书")
            .expect("证书合法");
        // 直接在 DER 里找 IP 的四字节形式（SAN 是扩展里的原始编码）。
        let needle = [192u8, 168, 1, 10];
        assert!(
            der.windows(needle.len()).any(|w| w == needle),
            "证书里应包含 192.168.1.10 的 IP SAN"
        );
        // 临时目录由 `TestDir` 的 Drop 回收（失败也会回收）；这里不再显式删除，
        // 免得同一件事有两套写法。
    }

    /// 只声明 http/1.1：不声明的话浏览器会带 h2 候选，虽然会回退，但语义不明确。
    #[test]
    fn alpn_is_http11_only() {
        let dir = test_dir("alpn");
        let material = load_or_generate(&dir, &hosts(), 30).expect("加载证书");
        assert_eq!(
            material.config.alpn_protocols,
            vec![b"http/1.1".to_vec()],
            "ALPN 必须只声明 http/1.1"
        );
        // 临时目录由 `TestDir` 的 Drop 回收（失败也会回收）；这里不再显式删除，
        // 免得同一件事有两套写法。
    }
}
