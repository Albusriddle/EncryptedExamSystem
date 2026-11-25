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
//!
//! 外部证书（`--tls-cert` / `--tls-key`）的**启动自检**在 `check.rs`（`check_external`）：
//! 在内存里跑一次真实握手，覆盖私钥配套 / EKU / 链 / 有效期 / SAN 覆盖；致命项由
//! `main()` 在**绑定端口之前**拒绝启动，警告项只提示（`--tls-check strict` 可让警告也拒绝）。

mod check;
mod material;
#[cfg(test)]
pub(crate) mod testutil;

pub use check::{CertCheck, check_external};
pub use material::{TlsMaterial, generate, load, load_or_generate};
// `main.rs` 用它做 tls_hosts 去重，保持 `pub(crate)`（拆分前就是 `pub(crate)`）。
pub(crate) use material::normalize_host;
