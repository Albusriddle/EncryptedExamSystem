//! 外部证书的启动自检：内存内跑一次真实握手，按「学生能不能打开页面」分级。

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{
    CertificateError, ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection,
};

use super::material::{read_material, unix_now};

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
    /// 证书 `notBefore` 在未来：典型成因是教师机时钟慢了（本仓 `timegate` 就是为
    /// 时钟不可信而存在的），浏览器允许「继续」，所以只告警。
    NotYetValid {
        now: u64,
        not_before: u64,
    },
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
            let pem = std::fs::read(ca).with_context(|| {
                format!(
                    "读取 CA 证书 {}（--tls-ca 应指向签发该证书的 CA 证书（PEM），不是私钥或目录）",
                    ca.display()
                )
            })?;
            let ca_certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut &pem[..])
                .collect::<Result<Vec<_>, _>>()
                .context("解析 CA 证书 PEM 失败")?;
            anyhow::ensure!(
                !ca_certs.is_empty(),
                "CA 文件里没有证书：{}（--tls-ca 要指向 CA 的证书 PEM，不是私钥）",
                ca.display()
            );
            let (added, ignored) = roots.add_parsable_certificates(ca_certs);
            anyhow::ensure!(
                added > 0,
                "CA 证书无法解析（忽略 {ignored} 张）：{}",
                ca.display()
            );
        }
        None => {
            // 与 CA 分支对称：这张证书若无法作为信任锚，所有地址都会报「链不可信」，
            // 而那会误导成「CA 配错了」。宁可在这里给一条准确的致命结论。
            let (added, _ignored) = roots.add_parsable_certificates(certs.iter().cloned());
            if added == 0 {
                return Ok(CertCheck {
                    fatals: vec![
                        concat!(
                            "证书无法用作信任锚（未给 --tls-ca 时以叶子证书为锚）。",
                            "修法：确认 --tls-cert 指向的是证书（PEM），不要指向私钥或目录"
                        )
                        .to_string(),
                    ],
                    ..CertCheck::default()
                });
            }
        }
    }

    let mut check = CertCheck::default();
    let mut expired_hosts: Vec<String> = Vec::new();
    let mut not_yet_valid: Option<(u64, u64)> = None;
    let mut not_yet_hosts: Vec<String> = Vec::new();
    let ca_given = ca_path.is_some();
    for host in hosts {
        match handshake_once(Arc::clone(&server_config), &roots, host) {
            Ok(()) => check.ok_hosts.push(host.clone()),
            Err(HandshakeFailure::Tls(e)) => match classify(&e) {
                CertIssue::Expired => expired_hosts.push(host.clone()),
                CertIssue::NotYetValid { now, not_before } => {
                    not_yet_valid.get_or_insert((now, not_before));
                    not_yet_hosts.push(host.clone());
                }
                CertIssue::NameMismatch => check.warnings.push(format!(
                    "证书 SAN 不覆盖 {host}：学生用这个地址访问会看到「名称不匹配」（仍可点继续）"
                )),
                CertIssue::MissingServerAuth => push_unique(
                    &mut check.fatals,
                    "证书的 EKU 不含 serverAuth：Chrome 会报不可绕过的 \
                     ERR_SSL_KEY_USAGE_INCOMPATIBLE，学生无法继续。\
                     修法：重签证书，在 EKU 里加上 serverAuth"
                        .to_string(),
                ),
                // `BadSignature` 有两种成因：私钥与证书不配套，或「信任锚选错」导致拿着
                // 别人的公钥去验签（两张自签证书 CN 相同、且给了错的 --tls-ca 时就会这样）。
                // 文案必须把两种都说出来，否则老师会去查一个并不存在的问题。
                CertIssue::KeyMismatch => push_unique(
                    &mut check.fatals,
                    "证书签名验证失败：私钥与证书不匹配，或信任锚不对。\
                     修法：确认 --tls-cert 与 --tls-key 是同一套；给了 --tls-ca 时确认证书确由该 CA 签发。\
                     学生侧会「连接被重置」而不是证书警告"
                        .to_string(),
                ),
                // 文案按「有没有给 --tls-ca」分岔：老师没传 CA 时，提示里不该出现
                // 「--tls-ca 指定的 CA」，那会把人指向一个他没做过的动作。
                CertIssue::ChainUntrusted => {
                    let msg = if ca_given {
                        "证书链接不到信任锚：这张证书不是 --tls-ca 指定的 CA 签发的，\
                         或证书文件里缺中间证书。\
                         修法：确认 --tls-ca 指向签发它的 CA 证书，并把证书文件按「叶子 → 中间 CA」顺序放全。\
                         学生侧会报「不是私密连接」且无法继续"
                    } else {
                        "证书链接不到信任锚：这张证书由 CA 签发，需要用 --tls-ca 指定签发它的 CA\
                         （只有自签证书才可以省略）。\
                         修法：用 --tls-ca 传入 CA 证书，并把证书文件按「叶子 → 中间 CA」顺序放全。\
                         学生侧会报「不是私密连接」且无法继续"
                    };
                    push_unique(&mut check.fatals, msg.to_string());
                }
                CertIssue::Other(msg) => push_unique(
                    &mut check.fatals,
                    format!("未分类的证书错误（把下面这行发给维护者）：{msg}"),
                ),
            },
            Err(HandshakeFailure::Io(msg)) => {
                push_unique(&mut check.fatals, format!("自检握手失败：{msg}"))
            }
        }
    }
    // 「尚未生效」与「已过期」都只告警（浏览器允许「继续」），但同类只报一条并列出地址。
    if let Some((now, not_before)) = not_yet_valid {
        check.warnings.insert(
            0,
            format!(
                "证书尚未生效（生效 {}，本机当前 {}）：多半是教师机时钟慢了，请先校时；\
                 学生点「继续」仍可进入（受影响地址：{}）",
                format_unix(not_before),
                format_unix(now),
                not_yet_hosts.join(", ")
            ),
        );
    }
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
    // 老师常从横幅 `https://[::1]:8443/` 直接复制，方括号是 URL 语法而不是地址的一部分。
    let trimmed = trimmed
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(trimmed);
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
        return Err(HandshakeFailure::Io(format!(
            "不是合法的地址/域名：{host}（IPv6 请去掉方括号，写成 ::1）"
        )));
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
                "自检握手未完成（链过长或协议异常：数据超过 256 KiB）".to_string(),
            ));
        }
    }
    Err(HandshakeFailure::Io(
        "自检握手未完成（链过长或协议异常：超过 8 轮）".to_string(),
    ))
}

fn classify(err: &rustls::Error) -> CertIssue {
    use CertificateError as C;
    match err {
        rustls::Error::InvalidCertificate(cert_err) => match cert_err {
            C::Expired | C::ExpiredContext { .. } => CertIssue::Expired,
            C::NotValidYet => CertIssue::NotYetValid {
                now: unix_now(),
                not_before: 0,
            },
            C::NotValidYetContext { time, not_before } => CertIssue::NotYetValid {
                now: time.as_secs(),
                not_before: not_before.as_secs(),
            },
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

/// 把 unix 秒格式化成「2026-11-25 14:00 UTC」。
///
/// 给老师看的提示里不能出现 `UnixTime(1795586479)` 这种内部结构——他既读不懂，
/// 也没法据此判断「是不是我时钟慢了」。
fn format_unix(secs: u64) -> String {
    match time::OffsetDateTime::from_unix_timestamp(secs as i64) {
        Ok(t) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02} UTC",
            t.year(),
            u8::from(t.month()),
            t.day(),
            t.hour(),
            t.minute()
        ),
        Err(_) => format!("{secs} 秒（无法格式化）"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tls::testutil::*;

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
        // 断言**类别**：过期、SAN 不覆盖也会「没过」，只断言 `!passed()` 抓不住分类回归。
        // 注意「给错 CA」在 rustls 里可能落成两种：同一曲线时 webpki 先做签名校验 →
        // `BadSignature`（归 KeyMismatch，文案已写明「或信任锚不对」）；CA 的 CN 不同时
        // 才是 `UnknownIssuer`。所以断言「落到信任锚这一类」，而不是某个具体变体。
        assert!(
            wrong.fatals.iter().any(|m| m.contains("信任锚")),
            "反例要落在「链/信任锚」这一类（不能是过期/SAN/EKU）：{:?}",
            wrong.fatals
        );
    }

    /// 证书 `notBefore` 在未来（教师机时钟慢了）→ **警告**，不是致命。
    ///
    /// 浏览器允许「继续」，拒绝启动会把一场本来能考的试卡死；文案还必须给出
    /// 可读时间与「校时钟」的提示，而不是 `NotValidYetContext { … }` 这种内部结构。
    #[test]
    fn external_cert_not_yet_valid_is_warning() {
        let dir = test_dir("ext-future");
        let now = time::OffsetDateTime::now_utc();
        let (cert, key) = make_cert(
            &dir,
            "future",
            &["127.0.0.1"],
            server_auth(),
            now + time::Duration::days(2),
            now + time::Duration::days(30),
            None,
        );
        let check = check_external(&cert, &key, None, &["127.0.0.1".to_string()]).expect("自检");
        assert!(
            check.fatals.is_empty(),
            "尚未生效不该判致命：{:?}",
            check.fatals
        );
        let joined = check.warnings.join("\n");
        assert!(joined.contains("尚未生效"), "文案要说明尚未生效：{joined}");
        assert!(joined.contains("UTC"), "时间要可读（带 UTC）：{joined}");
        assert!(
            !joined.contains("UnixTime("),
            "不能打印 rustls 内部结构：{joined}"
        );
        assert!(joined.contains("时钟"), "要给「校时」的提示：{joined}");
    }

    /// `--tls-host [::1]`（从横幅 `https://[::1]:8443/` 复制）要能用，不能判致命。
    #[test]
    fn external_cert_host_with_brackets_is_accepted() {
        let dir = test_dir("ext-bracket");
        let now = time::OffsetDateTime::now_utc();
        let (cert, key) = make_cert(
            &dir,
            "bracket",
            &["::1"],
            server_auth(),
            now - time::Duration::days(1),
            now + time::Duration::days(30),
            None,
        );
        let check = check_external(&cert, &key, None, &["[::1]".to_string()]).expect("自检");
        assert!(
            check.fatals.is_empty(),
            "带方括号的 IPv6 不该致命：{:?}",
            check.fatals
        );
        assert_eq!(check.ok_hosts, vec!["[::1]".to_string()]);
    }

    /// 所有给老师看的文案都不能含「连续两个空格」或 `{`（rustls Debug 结构）。
    ///
    /// 这条专门防 v1.6.0 出现过的那类回归：字符串折行没写 `\` 续行，21 个空格被打进提示里。
    #[test]
    fn external_cert_messages_have_no_formatting_artifacts() {
        let dir = test_dir("ext-msg");
        let now = time::OffsetDateTime::now_utc();
        let mut messages: Vec<String> = Vec::new();

        // 过期 + SAN 不覆盖
        let (expired, expired_key) = make_cert(
            &dir,
            "expired",
            &["127.0.0.1"],
            server_auth(),
            now - time::Duration::days(30),
            now - time::Duration::days(1),
            None,
        );
        let check = check_external(
            &expired,
            &expired_key,
            None,
            &["127.0.0.1".to_string(), "192.168.1.5".to_string()],
        )
        .expect("自检");
        messages.extend(check.fatals.iter().cloned());
        messages.extend(check.warnings.iter().cloned());

        // 缺 serverAuth（致命）
        let (no_eku, no_eku_key) = make_cert(
            &dir,
            "noeku",
            &["127.0.0.1"],
            vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth],
            now - time::Duration::days(1),
            now + time::Duration::days(30),
            None,
        );
        let check =
            check_external(&no_eku, &no_eku_key, None, &["127.0.0.1".to_string()]).expect("自检");
        messages.extend(check.fatals.iter().cloned());
        messages.extend(check.warnings.iter().cloned());

        // 链不可信（给了错的 CA）
        let (ca, _good_ca) = make_ca(&dir, "msg-good");
        let (_other, other_ca) = make_ca(&dir, "msg-bad");
        let (leaf, leaf_key) = make_cert(
            &dir,
            "msg-leaf",
            &["127.0.0.1"],
            server_auth(),
            now - time::Duration::days(1),
            now + time::Duration::days(30),
            Some(&ca),
        );
        let check = check_external(
            &leaf,
            &leaf_key,
            Some(&other_ca),
            &["127.0.0.1".to_string()],
        )
        .expect("自检");
        messages.extend(check.fatals.iter().cloned());
        messages.extend(check.warnings.iter().cloned());

        assert!(!messages.is_empty(), "应有可检查的文案");
        for msg in &messages {
            assert!(
                !msg.contains("  "),
                "文案里不能有连续空格（折行续写漏了 `\\`）：{msg}"
            );
            assert!(
                !msg.contains('{'),
                "文案里不能出现 rustls 的 Debug 结构：{msg}"
            );
        }
    }
}
