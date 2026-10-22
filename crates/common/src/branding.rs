//! 品牌信息：仓库地址、许可证、致谢名单。
//!
//! 单一来源是仓库根的 `branding.json`，编译期用 `include_str!` 注入。
//! 管理端（Python）读同一个文件，两端文案不会漂移。

use std::sync::OnceLock;

/// 原始 JSON 文本（编译期注入）。
pub const BRANDING_JSON: &str = include_str!("../../../branding.json");

/// 单条致谢。
#[derive(Debug, Clone, serde::Deserialize, PartialEq, Eq)]
pub struct Credit {
    pub role: String,
    pub name: String,
    pub url: String,
}

/// 品牌信息。
#[derive(Debug, Clone, serde::Deserialize, PartialEq, Eq)]
pub struct Branding {
    pub product: String,
    pub abbr: String,
    pub repo_url: String,
    pub license: String,
    pub license_name: String,
    pub tagline: String,
    pub credits_title: String,
    pub credits: Vec<Credit>,
    pub star_hint: String,
}

impl Branding {
    /// 形如 `保密考试系统（EES）`。
    pub fn display_name(&self) -> String {
        format!("{}（{}）", self.product, self.abbr)
    }
}

/// 取品牌信息。解析失败时 panic：这属于构建期错误，
/// `branding_parses` 测试与构建自检会先拦住。
pub fn branding() -> &'static Branding {
    static CACHE: OnceLock<Branding> = OnceLock::new();
    CACHE.get_or_init(|| serde_json::from_str(BRANDING_JSON).expect("branding.json 解析失败"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branding_parses() {
        let b = branding();
        assert!(!b.product.is_empty());
        assert!(!b.tagline.is_empty());
        assert!(!b.credits.is_empty(), "致谢名单不能为空");
    }

    #[test]
    fn repo_url_is_the_expected_one() {
        assert_eq!(
            branding().repo_url,
            "https://github.com/Albusriddle/EncryptedExamSystem"
        );
    }

    /// 弹窗里声明的许可证必须与仓库 LICENSE 文件一致。
    #[test]
    fn license_matches_license_file() {
        let text = include_str!("../../../LICENSE");
        let declared = &branding().license_name;
        assert!(
            text.contains("Apache License") && declared.contains("Apache"),
            "branding 声明 {declared} 与 LICENSE 文件不符"
        );
    }

    #[test]
    fn credits_contain_required_people() {
        let names: Vec<&str> = branding().credits.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"AlbusRiddle"), "缺少创始人: {names:?}");
        assert!(names.contains(&"Stone"), "缺少 Stone: {names:?}");
        assert!(
            names.iter().any(|n| n.contains("Robot")),
            "缺少湖南大学 Robot 工作坊: {names:?}"
        );
        for c in &branding().credits {
            assert!(
                c.url.starts_with("https://github.com/"),
                "链接不合法: {}",
                c.url
            );
        }
    }

    #[test]
    fn star_hint_mentions_star() {
        assert!(branding().star_hint.contains("Star") || branding().star_hint.contains("star"));
    }
}
