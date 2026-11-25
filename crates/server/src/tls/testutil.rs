//! 测试辅助：临时目录守卫 + 现造证书/CA。
//!
//! 抽到独立文件是因为 `material.rs` 与 `check.rs` 的用例都要用它们；
//! 以前它们散在 `mod tests` 里，两边都得复制一份。

use std::path::{Path, PathBuf};

/// 测试目录守卫：Drop 时删目录。
///
/// 以前是「每条用例末尾 `remove_dir_all`」——断言一失败就漏清理，
/// `target/server-tests/tls-*` 会越积越多。`Deref<Target = Path>` 让既有写法
/// （`&dir`、`dir.join(...)`）不用改。
///
/// 注意：v1.7.0 之前留下的 `target/server-tests/tls-*`（以及 `sweep-lock-*`）
/// 不会被这个守卫回收——它只管本次运行创建的那一个。想清就手工删 `target/server-tests/`。
pub(crate) struct TestDir(PathBuf);

impl std::ops::Deref for TestDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

// `std::fs::*` 的泛型参数要的是 `AsRef<Path>`，光有 `Deref` 不够。
impl AsRef<Path> for TestDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn test_dir(tag: &str) -> TestDir {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/server-tests")
        .join(format!("tls-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    TestDir(dir)
}

/// 造一张证书并落盘，返回 (证书路径, 私钥路径)。
///
/// 参数多是有意的：自检要覆盖「EKU / 有效期 / SAN / 签名者」四种差异，
/// 每类都单独写一遍 rcgen 样板只会让差异埋进噪音里。
#[allow(clippy::too_many_arguments)]
pub(crate) fn make_cert(
    dir: &Path,
    tag: &str,
    hosts: &[&str],
    eku: Vec<rcgen::ExtendedKeyUsagePurpose>,
    not_before: time::OffsetDateTime,
    not_after: time::OffsetDateTime,
    signer: Option<&rcgen::Certificate>,
) -> (PathBuf, PathBuf) {
    let mut params =
        rcgen::CertificateParams::new(hosts.iter().map(|h| (*h).to_string()).collect::<Vec<_>>());
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
pub(crate) fn make_ca(dir: &Path, tag: &str) -> (rcgen::Certificate, PathBuf) {
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

pub(crate) fn server_auth() -> Vec<rcgen::ExtendedKeyUsagePurpose> {
    vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth]
}
