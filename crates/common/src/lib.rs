//! 保密考试系统共享核心库：加密、密钥派生、确定性抽题、包格式、时间门禁。
//!
//! 被 builder（打包端）和 client（学生端）共同依赖，保证两端算法完全一致。

pub mod branding;
pub mod container;
pub mod crypto;
pub mod model;
pub mod picker;
pub mod timegate;

pub use branding::branding;
pub use model::*;

/// 版本号，编译期从仓库根的 `VERSION` 文件注入。
///
/// 单一来源：Cargo.toml / admin/pyproject.toml / 发布 tag 都以它为基准，
/// `tests/test_version.py` 与 release 工作流会校验一致性。
pub const VERSION_RAW: &str = include_str!("../../../VERSION");

/// 版本号（去掉文件末尾换行）。
pub fn version() -> &'static str {
    VERSION_RAW.trim()
}

/// 产品水印：`EES vX.Y.Z Powered by AlbusRiddle`。
///
/// 管理端工具栏与学生端顶部都显示同一串，便于用户确认版本、
/// 也方便排障时让用户直接报版本号。
///
/// 品牌名与作者取自 `branding.json`（与弹窗同一来源），
/// 不再硬编码——改了 branding 水印会跟着变。
pub fn watermark() -> String {
    let b = branding::branding();
    let author = b
        .credits
        .first()
        .map(|c| c.name.as_str())
        .unwrap_or("AlbusRiddle");
    format!("{} v{} Powered by {}", b.abbr, version(), author)
}

#[cfg(test)]
mod version_tests {
    use super::*;

    /// 水印必须由 branding.json 驱动，不能硬编码。
    #[test]
    fn watermark_uses_branding() {
        let b = branding::branding();
        let author = &b.credits[0].name;
        let expected = format!("{} v{} Powered by {}", b.abbr, version(), author);
        assert_eq!(watermark(), expected);
    }

    #[test]
    fn version_is_non_empty_and_clean() {
        let v = version();
        assert!(!v.is_empty(), "VERSION 文件为空");
        assert_eq!(v, v.trim(), "VERSION 不应含首尾空白");
        assert!(
            v.chars().next().unwrap().is_ascii_digit(),
            "版本应以数字开头"
        );
        assert!(!v.contains('\n'), "版本不应含换行");
    }

    #[test]
    fn watermark_format() {
        let w = watermark();
        assert!(w.starts_with("EES v"), "实际: {w}");
        assert!(w.ends_with("Powered by AlbusRiddle"), "实际: {w}");
        assert!(w.contains(version()));
    }
}
