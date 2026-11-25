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

use std::path::PathBuf;
use std::sync::Arc;

use rustls::ServerConfig;

mod check;
mod material;
#[cfg(test)]
pub(crate) mod testutil;

pub use check::check_external;
pub use material::{generate, load, load_or_generate};
// 拆分前 `crate::tls::` 下就有的两个名字：`CertCheck` 只作为 `check_external` 的返回类型
// 被用到（没人写全名），`fingerprint_hex` 只有 tls 内部用——保留在公开路径上是为了
// 拆分前后对外可见面一致；bin crate 里它们没有外部调用点，故按需静音。
pub use check::CertCheck;
// `main.rs` 用它做 tls_hosts 去重，保持 `pub(crate)`（拆分前就是 `pub(crate)`）。
pub(crate) use material::normalize_host;

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
