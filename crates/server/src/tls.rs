//! 自签 TLS：生成/加载证书、算指纹。
//!
//! 为什么是自签：教师机只有局域网 IP，拿不到公共 CA 的证书（Let's Encrypt 不给私网 IP 签发，
//! 考场也没有外网）。往学生私有设备上装根 CA 风险太大——等于把「伪造任意 HTTPS 站点」的
//! 能力留给教师机上的那把私钥。自签 + 学生首次点「高级 → 继续」能挡住同网段**被动嗅探**
//! （这是考场最常见的威胁）；要防**中间人**，靠教师屏显示指纹让学生核对。
//!
//! 证书必须满足「Chrome 允许点继续」：SAN 含 IP、EKU 含 serverAuth、keyUsage 合规。
//! 缺 EKU 会报 `ERR_SSL_KEY_USAGE_INCOMPATIBLE`，那个错误**不可绕过**。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

use crate::secure_fs;

const CERT_FILE: &str = "server.crt";
const KEY_FILE: &str = "server.key";

/// 已就绪的 TLS 材料。
pub struct TlsMaterial {
    pub config: Arc<ServerConfig>,
    /// 证书 SHA-256 指纹（大写十六进制、冒号分隔）。
    pub fingerprint: String,
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
    /// 是不是本次启动新生成的（横幅里要提示「已自动生成」）。
    pub generated: bool,
}

/// 加载已有证书；没有就生成一张（有效期 `days` 天）。
pub fn load_or_generate(dir: &Path, hosts: &[String], days: i64) -> Result<TlsMaterial> {
    let cert_path = dir.join(CERT_FILE);
    let key_path = dir.join(KEY_FILE);
    if cert_path.is_file() && key_path.is_file() {
        return load(&cert_path, &key_path);
    }
    generate(dir, hosts, days)?;
    let mut material = load(&cert_path, &key_path)?;
    material.generated = true;
    Ok(material)
}

/// 加载指定路径的证书与私钥（老师用自己的 CA 签的证书时走这里）。
pub fn load(cert_path: &Path, key_path: &Path) -> Result<TlsMaterial> {
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

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs.clone(), key)
        .context("构建 TLS 配置失败（证书与私钥不匹配？）")?;
    Ok(TlsMaterial {
        config: Arc::new(config),
        fingerprint: fingerprint_hex(certs[0].as_ref()),
        cert_path: cert_path.to_path_buf(),
        key_path: key_path.to_path_buf(),
        generated: false,
    })
}

/// 生成一张自签证书（ECDSA P-256）。
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
    std::fs::write(&cert_path, cert_pem.as_bytes())
        .with_context(|| format!("写入证书 {}", cert_path.display()))?;
    std::fs::write(&key_path, key_pem.as_bytes())
        .with_context(|| format!("写入私钥 {}", key_path.display()))?;
    // 私钥必须只给当前用户（内容虽不是 master，但泄漏=可冒充考场服务端）。
    secure_fs::harden_file(&key_path).context("收紧私钥权限失败")?;
    let _ = secure_fs::harden_file(&cert_path);
    Ok(())
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

    /// 生成后能立刻加载，且指纹是 32 字节的冒号分隔大写十六进制。
    #[test]
    fn generated_cert_loads_and_has_fingerprint() {
        let dir = test_dir("gen");
        let hosts = vec!["127.0.0.1".to_string(), "192.168.1.10".to_string()];
        generate(&dir, &hosts, 30).expect("生成证书");
        let material = load_or_generate(&dir, &hosts, 30).expect("加载证书");
        assert!(!material.generated, "第二次应复用已有证书");
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
}
