//! 自签 TLS：生成/加载证书、算指纹、判断要不要重签。
//!
//! 为什么是自签：教师机只有局域网 IP，拿不到公共 CA 的证书（Let's Encrypt 不给私网 IP 签发，
//! 考场也没有外网）。往学生私有设备上装根 CA 风险太大——等于把「伪造任意 HTTPS 站点」的
//! 能力留给教师机上的那把私钥。自签 + 学生首次点「高级 → 继续」能挡住同网段**被动嗅探**
//! （这是考场最常见的威胁）；要防**中间人**，靠教师屏显示指纹让学生核对。
//!
//! 证书必须满足「Chrome 允许点继续」：SAN 含 IP、EKU 含 serverAuth、keyUsage 合规。
//! 缺 EKU 会报 `ERR_SSL_KEY_USAGE_INCOMPATIBLE`，那个错误**不可绕过**。
//!
//! 重签策略（v1.5.1）：自签证书可以丢（学生本来就点「继续」），所以**宁可重签**——
//! 到期前 30 天、或本机地址变了（SAN 不含新地址）、或元数据缺失（旧版本目录），都重签。
//! 这解决了两类现场事故：① 第 366 天起全班看到「证书已过期」；② 教师机换网段后
//! 学生看到「名称不匹配」——而老师根本不会想到要去删 `tls/` 目录。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{
    CertificateError, ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection,
};

use crate::secure_fs;

const CERT_FILE: &str = "server.crt";
const KEY_FILE: &str = "server.key";
/// 证书的元数据（到期时间 + SAN）。没有它就不知道现有证书是否还适用。
const META_FILE: &str = "server.json";
/// 到期前这么多天就重签：留出「考试当天才发现过期」的余量。
const RENEW_BEFORE_SECS: u64 = 30 * 24 * 3600;

/// 已就绪的 TLS 材料。
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
/// [`check_external`]（在 `main()` 里、绑定端口之前跑），这样「拒绝启动」发生在
/// 任何监听建立之前，也不会把两种错误混在一条消息里。
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
fn read_material(
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

/// 外部证书的启动自检结论。
///
/// 分级而不是「一有问题就不让启动」，是因为两类失败的后果完全不同：
/// - `fatals`：学生**根本连不上**（私钥不匹配 → 连接被重置；缺 serverAuth → Chrome 报
///   不可绕过的 `ERR_SSL_KEY_USAGE_INCOMPATIBLE`；链不可用）。这种配置晚发现一小时，
///   就是整个考场白等一小时，所以宁可**拒绝启动**。
/// - `warnings`：学生仍能点「继续」（过期、SAN 不覆盖某个地址）。硬拒绝反而更糟
///   （考试开不起来），所以只告警，由老师判断——需要严格时用 `--tls-check strict`。
#[derive(Debug, Default)]
pub struct CertCheck {
    pub fatals: Vec<String>,
    pub warnings: Vec<String>,
    /// 通过校验的地址（横幅上要列出来，让老师知道「这些地址学生不会看到名称不匹配」）。
    pub ok_hosts: Vec<String>,
}

impl CertCheck {
    pub fn passed(&self) -> bool {
        self.fatals.is_empty()
    }
}

/// 自检用的单次握手上限：正常 TLS 1.3 握手 2 轮就结束，留 8 轮足够；
/// 字节上限防「实现写错 → 两个 Connection 互喂死循环」。
const HANDSHAKE_MAX_ROUNDS: usize = 8;
const HANDSHAKE_MAX_BYTES: usize = 256 * 1024;

/// 一次握手失败的原因。
///
/// 为什么不直接用 `rustls::Error`：`rustls` 0.23 的 `Error` 里**没有 IO 变体**
/// （`read_tls`/`write_tls` 返回 `io::Error`），而自检要把「TLS 层可分类的错误」
/// 与「IO/构造失败」分开处理。
enum HandshakeFailure {
    Tls(Box<rustls::Error>),
    Io(String),
}

/// 证书问题分类（只覆盖与「学生能不能打开页面」相关的几类）。
enum CertIssue {
    Expired,
    NameMismatch,
    MissingServerAuth,
    KeyMismatch,
    /// 证书链接不到信任锚（`--tls-ca` 给错、或证书其实是自签的却没给 CA）。
    ChainUntrusted,
    Other(String),
}

/// 为什么用「内存内握手」而不是解析 X.509：
///
/// 解析证书只能看到字段，**看不到浏览器的判定结果**；而我们要回答的正是
/// 「学生打开页面会看到什么」。握手走的就是 webpki 的真实校验路径
/// （签名、有效期、SAN、EKU、链），一次就能同时覆盖 5 类问题，且**零新依赖**——
/// 只用已经在依赖里的 `rustls`。
pub fn check_external(
    cert_path: &Path,
    key_path: &Path,
    ca_path: Option<&Path>,
    hosts: &[String],
) -> Result<CertCheck> {
    let (certs, key) = read_material(cert_path, key_path)?;
    // rustls 组配置时就会检查「私钥与证书是否配套」（`KeyMismatch`）。这属于
    // 「这套证书没法用」，是自检要**报告**的致命项，不是读取失败——所以这里不 `?`，
    // 而是直接产出一条 fatal，让 main() 按统一口径拒绝启动。
    let mut server_config = match ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs.clone(), key)
    {
        Ok(config) => config,
        Err(e) => {
            return Ok(CertCheck {
                fatals: vec![format!(
                    "私钥与证书不匹配或证书不可用（{e}）：学生侧会「连接被重置」而不是证书警告"
                )],
                ..CertCheck::default()
            });
        }
    };
    // 与本服务真实配置一致：只声明 http/1.1（ALPN 不匹配也会让某些客户端握手失败）。
    server_config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let server_config = Arc::new(server_config);

    // 信任锚：给了 `--tls-ca` 就只信它（这样才能验出「证书不是这张 CA 签的」）；
    // 没给就把叶子证书当锚——「自签证书 / 单张证书」场景下这是唯一能验私钥匹配的办法。
    let mut roots = RootCertStore::empty();
    match ca_path {
        Some(ca) => {
            let pem =
                std::fs::read(ca).with_context(|| format!("读取 CA 证书 {}", ca.display()))?;
            let ca_certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut &pem[..])
                .collect::<Result<Vec<_>, _>>()
                .context("解析 CA 证书 PEM 失败")?;
            anyhow::ensure!(!ca_certs.is_empty(), "CA 文件里没有证书：{}", ca.display());
            let (added, ignored) = roots.add_parsable_certificates(ca_certs);
            anyhow::ensure!(
                added > 0,
                "CA 证书无法解析（忽略 {ignored} 张）：{}",
                ca.display()
            );
        }
        None => {
            roots.add_parsable_certificates(certs.iter().cloned());
        }
    }

    let mut check = CertCheck::default();
    let mut expired_hosts: Vec<String> = Vec::new();
    for host in hosts {
        match handshake_once(Arc::clone(&server_config), &roots, host) {
            Ok(()) => check.ok_hosts.push(host.clone()),
            Err(HandshakeFailure::Tls(e)) => match classify(&e) {
                CertIssue::Expired => expired_hosts.push(host.clone()),
                CertIssue::NameMismatch => check.warnings.push(format!(
                    "证书 SAN 不覆盖 {host}：学生用这个地址访问会看到「名称不匹配」（仍可点继续）"
                )),
                CertIssue::MissingServerAuth => push_unique(
                    &mut check.fatals,
                    "证书的 EKU 不含 serverAuth：Chrome 会报不可绕过的 \
                     ERR_SSL_KEY_USAGE_INCOMPATIBLE，学生无法继续"
                        .to_string(),
                ),
                // `BadSignature` 有两种成因：私钥与证书不配套，或「信任锚选错」导致拿着
                // 别人的公钥去验签（两张自签证书 CN 相同、且给了错的 --tls-ca 时就会这样）。
                // 文案必须把两种都说出来，否则老师会去查一个并不存在的问题。
                CertIssue::KeyMismatch => push_unique(
                    &mut check.fatals,
                    "证书签名验证失败：私钥与证书不匹配，或信任锚不对（给了 --tls-ca 时请确认证书确由该 CA 签发）。学生侧会「连接被重置」而不是证书警告"
                        .to_string(),
                ),
                CertIssue::ChainUntrusted => push_unique(
                    &mut check.fatals,
                    "证书链接不到信任锚：`--tls-ca` 指定的 CA 没签过这张证书（或不完整），                     学生侧会报「不是私密连接」且无法继续".to_string(),
                ),
                CertIssue::Other(msg) => {
                    push_unique(&mut check.fatals, format!("证书自检失败：{msg}"))
                }
            },
            Err(HandshakeFailure::Io(msg)) => {
                push_unique(&mut check.fatals, format!("自检握手失败：{msg}"))
            }
        }
    }
    // 过期只报一条（5 个地址都过期 = 同一件事），但把受影响的地址列出来。
    if !expired_hosts.is_empty() {
        check.warnings.insert(
            0,
            format!(
                "证书已过期（受影响地址：{}）：学生仍可点「继续」，但警告比自签更吓人",
                expired_hosts.join(", ")
            ),
        );
    }
    Ok(check)
}

fn push_unique(list: &mut Vec<String>, msg: String) {
    if !list.contains(&msg) {
        list.push(msg);
    }
}

/// 把「地址/域名」转成 `ServerName`（IP 走 IpAddress，否则走 DnsName）。
fn server_name_of(host: &str) -> Option<ServerName<'static>> {
    let trimmed = host.trim();
    if let Ok(ip) = trimmed.parse::<std::net::IpAddr>() {
        return Some(ServerName::IpAddress(ip.into()));
    }
    ServerName::try_from(trimmed.to_ascii_lowercase()).ok()
}

/// 在内存里跑一次客户端↔服务端握手（不开端口、不走网络）。
fn handshake_once(
    server_config: Arc<ServerConfig>,
    roots: &RootCertStore,
    host: &str,
) -> std::result::Result<(), HandshakeFailure> {
    let Some(name) = server_name_of(host) else {
        return Err(HandshakeFailure::Io(format!("不是合法的地址/域名：{host}")));
    };
    let client_config = ClientConfig::builder()
        .with_root_certificates(roots.clone())
        .with_no_client_auth();
    let mut client = ClientConnection::new(Arc::new(client_config), name)
        .map_err(|e| HandshakeFailure::Tls(Box::new(e)))?;
    let mut server =
        ServerConnection::new(server_config).map_err(|e| HandshakeFailure::Tls(Box::new(e)))?;

    let mut c2s: Vec<u8> = Vec::new();
    let mut s2c: Vec<u8> = Vec::new();
    let mut total = 0usize;
    for _ in 0..HANDSHAKE_MAX_ROUNDS {
        // client → server
        c2s.clear();
        while client.wants_write() {
            let n = client
                .write_tls(&mut c2s)
                .map_err(|e| HandshakeFailure::Io(format!("客户端写：{e}")))?;
            if n == 0 {
                break;
            }
        }
        if !c2s.is_empty() {
            total += c2s.len();
            let mut rest: &[u8] = &c2s;
            while !rest.is_empty() {
                let n = server
                    .read_tls(&mut rest)
                    .map_err(|e| HandshakeFailure::Io(format!("服务端读：{e}")))?;
                if n == 0 {
                    break;
                }
            }
            server
                .process_new_packets()
                .map_err(|e| HandshakeFailure::Tls(Box::new(e)))?;
        }

        // server → client（客户端在这一步校验证书）
        s2c.clear();
        while server.wants_write() {
            let n = server
                .write_tls(&mut s2c)
                .map_err(|e| HandshakeFailure::Io(format!("服务端写：{e}")))?;
            if n == 0 {
                break;
            }
        }
        if !s2c.is_empty() {
            total += s2c.len();
            let mut rest: &[u8] = &s2c;
            while !rest.is_empty() {
                let n = client
                    .read_tls(&mut rest)
                    .map_err(|e| HandshakeFailure::Io(format!("客户端读：{e}")))?;
                if n == 0 {
                    break;
                }
            }
            client
                .process_new_packets()
                .map_err(|e| HandshakeFailure::Tls(Box::new(e)))?;
        }

        if !client.is_handshaking() && !server.is_handshaking() {
            return Ok(());
        }
        if total > HANDSHAKE_MAX_BYTES {
            return Err(HandshakeFailure::Io(
                "自检握手数据超过 256 KiB（实现有误？）".to_string(),
            ));
        }
    }
    Err(HandshakeFailure::Io(
        "自检握手没有在限定轮数内完成".to_string(),
    ))
}

fn classify(err: &rustls::Error) -> CertIssue {
    use CertificateError as C;
    match err {
        rustls::Error::InvalidCertificate(cert_err) => match cert_err {
            C::Expired | C::ExpiredContext { .. } => CertIssue::Expired,
            C::NotValidForName | C::NotValidForNameContext { .. } => CertIssue::NameMismatch,
            // rustls 0.23 里**没有** `RequiredEkuNotFound`：webpki 的
            // `Error::RequiredEkuNotFound` 被映射成 `InvalidPurpose`
            // （见 rustls `src/webpki/mod.rs`），所以这里按 `InvalidPurpose` 分类。
            C::InvalidPurpose | C::InvalidPurposeContext { .. } => CertIssue::MissingServerAuth,
            C::BadSignature => CertIssue::KeyMismatch,
            // 链接不到信任锚：`--tls-ca` 给错、或证书是自签的而没给 CA。
            C::UnknownIssuer => CertIssue::ChainUntrusted,
            other => CertIssue::Other(format!("{other:?}")),
        },
        other => CertIssue::Other(format!("{other}")),
    }
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

fn normalize_host(host: &str) -> String {
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

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 证书指纹：SHA-256，大写十六进制、冒号分隔（与浏览器「证书详情」里一致）。
pub fn fingerprint_hex(der: &[u8]) -> String {
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

    fn test_dir(tag: &str) -> PathBuf {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/server-tests")
            .join(format!("tls-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

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
        let _ = std::fs::remove_dir_all(&dir);
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
        let _ = std::fs::remove_dir_all(&dir);
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
        let _ = std::fs::remove_dir_all(&dir);
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
        let _ = std::fs::remove_dir_all(&dir);
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
        let _ = std::fs::remove_dir_all(&dir);
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
        let _ = std::fs::remove_dir_all(&dir);
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
        let _ = std::fs::remove_dir_all(&dir);
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
        let _ = std::fs::remove_dir_all(&dir);
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
        let _ = std::fs::remove_dir_all(&dir);
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
        let _ = std::fs::remove_dir_all(&dir);
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
        let _ = std::fs::remove_dir_all(&dir);
    }
    // ------------------------------------------------------------ 外部证书自检

    /// 造一张证书并落盘，返回 (证书路径, 私钥路径)。
    ///
    /// 参数多是有意的：自检要覆盖「EKU / 有效期 / SAN / 签名者」四种差异，
    /// 每类都单独写一遍 rcgen 样板只会让差异埋进噪音里。
    #[allow(clippy::too_many_arguments)]
    fn make_cert(
        dir: &Path,
        tag: &str,
        hosts: &[&str],
        eku: Vec<rcgen::ExtendedKeyUsagePurpose>,
        not_before: time::OffsetDateTime,
        not_after: time::OffsetDateTime,
        signer: Option<&rcgen::Certificate>,
    ) -> (PathBuf, PathBuf) {
        let mut params = rcgen::CertificateParams::new(
            hosts.iter().map(|h| (*h).to_string()).collect::<Vec<_>>(),
        );
        params.alg = &rcgen::PKCS_ECDSA_P256_SHA256;
        params.is_ca = rcgen::IsCa::NoCa;
        params.key_usages = vec![
            rcgen::KeyUsagePurpose::DigitalSignature,
            rcgen::KeyUsagePurpose::KeyEncipherment,
        ];
        params.extended_key_usages = eku;
        params.not_before = not_before;
        params.not_after = not_after;
        let cert = rcgen::Certificate::from_params(params).expect("签证书");
        let cert_pem = match signer {
            Some(ca) => cert.serialize_pem_with_signer(ca).expect("CA 签发"),
            None => cert.serialize_pem().expect("自签"),
        };
        let key_pem = cert.serialize_private_key_pem();
        std::fs::create_dir_all(dir).expect("建目录");
        let cert_path = dir.join(format!("{tag}.crt"));
        let key_path = dir.join(format!("{tag}.key"));
        std::fs::write(&cert_path, cert_pem).expect("写证书");
        std::fs::write(&key_path, key_pem).expect("写私钥");
        (cert_path, key_path)
    }

    /// 造一张 CA（BasicConstraints CA:TRUE + keyCertSign）。
    fn make_ca(dir: &Path, tag: &str) -> (rcgen::Certificate, PathBuf) {
        let now = time::OffsetDateTime::now_utc();
        let mut params = rcgen::CertificateParams::new(vec![format!("ca-{tag}")]);
        params.alg = &rcgen::PKCS_ECDSA_P256_SHA256;
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params.key_usages = vec![
            rcgen::KeyUsagePurpose::KeyCertSign,
            rcgen::KeyUsagePurpose::CrlSign,
            rcgen::KeyUsagePurpose::DigitalSignature,
        ];
        params.not_before = now - time::Duration::days(1);
        params.not_after = now + time::Duration::days(365);
        let cert = rcgen::Certificate::from_params(params).expect("签 CA");
        std::fs::create_dir_all(dir).expect("建目录");
        let path = dir.join(format!("{tag}-ca.crt"));
        std::fs::write(&path, cert.serialize_pem().expect("CA PEM")).expect("写 CA");
        (cert, path)
    }

    fn server_auth() -> Vec<rcgen::ExtendedKeyUsagePurpose> {
        vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth]
    }

    /// 匹配的证书 + 私钥 + SAN 覆盖 → 通过，且记下覆盖的地址。
    #[test]
    fn external_cert_with_matching_key_passes() {
        let dir = test_dir("ext-ok");
        let now = time::OffsetDateTime::now_utc();
        let (cert, key) = make_cert(
            &dir,
            "ok",
            &["127.0.0.1", "192.168.1.10"],
            server_auth(),
            now - time::Duration::days(1),
            now + time::Duration::days(30),
            None,
        );
        let hosts = vec!["127.0.0.1".to_string(), "192.168.1.10".to_string()];
        let check = check_external(&cert, &key, None, &hosts).expect("自检");
        assert!(check.fatals.is_empty(), "不该有致命项：{:?}", check.fatals);
        assert!(
            check.warnings.is_empty(),
            "不该有警告：{:?}",
            check.warnings
        );
        assert_eq!(check.ok_hosts, hosts, "两个地址都该通过");
        assert!(check.passed());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 证书与私钥不是一对 → 致命（学生侧表现为「连接被重置」，最难排查）。
    #[test]
    fn external_cert_key_mismatch_is_fatal() {
        let dir = test_dir("ext-mismatch");
        let now = time::OffsetDateTime::now_utc();
        let (a_cert, _a_key) = make_cert(
            &dir,
            "a",
            &["127.0.0.1"],
            server_auth(),
            now - time::Duration::days(1),
            now + time::Duration::days(30),
            None,
        );
        let (_b_cert, b_key) = make_cert(
            &dir,
            "b",
            &["127.0.0.1"],
            server_auth(),
            now - time::Duration::days(1),
            now + time::Duration::days(30),
            None,
        );
        let check =
            check_external(&a_cert, &b_key, None, &["127.0.0.1".to_string()]).expect("自检");
        assert!(!check.passed(), "私钥与证书不匹配必须判致命");
        assert!(
            check.fatals.iter().any(|m| m.contains("不匹配")),
            "文案要点明「不匹配」：{:?}",
            check.fatals
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 已过期 → 只告警（浏览器仍可「继续」），且只报一条并列出受影响地址。
    #[test]
    fn external_cert_expired_is_warning() {
        let dir = test_dir("ext-expired");
        let now = time::OffsetDateTime::now_utc();
        let (cert, key) = make_cert(
            &dir,
            "exp",
            &["127.0.0.1"],
            server_auth(),
            now - time::Duration::days(30),
            now - time::Duration::days(1),
            None,
        );
        let check = check_external(&cert, &key, None, &["127.0.0.1".to_string()]).expect("自检");
        assert!(check.fatals.is_empty(), "过期不该致命：{:?}", check.fatals);
        assert!(
            check.warnings.iter().any(|m| m.contains("过期")),
            "文案要含「过期」：{:?}",
            check.warnings
        );
        assert_eq!(
            check.warnings.iter().filter(|m| m.contains("过期")).count(),
            1,
            "过期只报一条：{:?}",
            check.warnings
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// SAN 不覆盖某个地址 → 该地址一条警告（其它地址仍可通过）。
    #[test]
    fn external_cert_name_mismatch_is_warning() {
        let dir = test_dir("ext-name");
        let now = time::OffsetDateTime::now_utc();
        let (cert, key) = make_cert(
            &dir,
            "name",
            &["127.0.0.1"],
            server_auth(),
            now - time::Duration::days(1),
            now + time::Duration::days(30),
            None,
        );
        let check = check_external(
            &cert,
            &key,
            None,
            &["127.0.0.1".to_string(), "192.168.1.5".to_string()],
        )
        .expect("自检");
        assert!(
            check.fatals.is_empty(),
            "SAN 不覆盖不该致命：{:?}",
            check.fatals
        );
        assert_eq!(check.ok_hosts, vec!["127.0.0.1".to_string()]);
        assert!(
            check.warnings.iter().any(|m| m.contains("192.168.1.5")),
            "警告要指名那个地址：{:?}",
            check.warnings
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 缺 EKU serverAuth → 致命（Chrome 报不可绕过的 ERR_SSL_KEY_USAGE_INCOMPATIBLE）。
    #[test]
    fn external_cert_missing_server_auth_is_fatal() {
        let dir = test_dir("ext-eku");
        let now = time::OffsetDateTime::now_utc();
        let (cert, key) = make_cert(
            &dir,
            "eku",
            &["127.0.0.1"],
            vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth],
            now - time::Duration::days(1),
            now + time::Duration::days(30),
            None,
        );
        let check = check_external(&cert, &key, None, &["127.0.0.1".to_string()]).expect("自检");
        assert!(!check.passed(), "缺 serverAuth 必须判致命");
        assert!(
            check.fatals.iter().any(|m| m.contains("serverAuth")),
            "文案要含 serverAuth：{:?}",
            check.fatals
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// 给了 `--tls-ca` 就用它当信任锚：正确的 CA 通过，不相关的 CA 判致命（链不可用）。
    #[test]
    fn external_cert_chain_is_checked_against_given_ca() {
        let dir = test_dir("ext-ca");
        let now = time::OffsetDateTime::now_utc();
        let (ca, ca_path) = make_ca(&dir, "good");
        let (_other_ca, other_ca_path) = make_ca(&dir, "bad");
        let (cert, key) = make_cert(
            &dir,
            "leaf",
            &["127.0.0.1"],
            server_auth(),
            now - time::Duration::days(1),
            now + time::Duration::days(30),
            Some(&ca),
        );
        let hosts = vec!["127.0.0.1".to_string()];

        let ok = check_external(&cert, &key, Some(&ca_path), &hosts).expect("自检");
        assert!(ok.fatals.is_empty(), "CA 正确时不该致命：{:?}", ok.fatals);
        assert_eq!(ok.ok_hosts, hosts);

        let wrong = check_external(&cert, &key, Some(&other_ca_path), &hosts).expect("自检");
        assert!(!wrong.passed(), "不是这张 CA 签的必须判致命");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
