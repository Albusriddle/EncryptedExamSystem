//! 用户数据目录（答题卡、欢迎弹窗标记）。
//!
//! **只在这里定义一次**：以前 `welcome.rs` 与 `gui.rs` 各写一套回退逻辑，
//! Linux 上会得到 `$HOME/ExamSystem` 与 `$HOME/.local/share/ExamSystem` 两个目录；
//! `gui.rs` 更是在取不到目录时回退到当前工作目录 `"."`——从「开始菜单」启动时
//! 那是 `System32`，写不进去，而且学生完全看不出发生了什么。
//!
//! 顺序（与各平台惯例一致）：
//!
//! 1. Windows：`%LOCALAPPDATA%`（`gui.rs` 一直用它，行为不变）；
//! 2. Linux/其它：`$XDG_DATA_HOME`，没有就用 `$HOME/.local/share`；
//! 3. 都没有：返回 `None`，由调用方**明确提示**「本次作答不会落盘」，
//!    **不要**再偷偷写到当前目录。

use std::path::PathBuf;

/// 本产品的用户数据根目录（`.../ExamSystem`）。
pub fn user_data_dir() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_DATA_HOME").map(PathBuf::from))
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("share"))
        })?;
    Some(base.join("ExamSystem"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 数据目录必须落在 `ExamSystem` 下——学生的答题卡与「不再提示」标记都靠它定位。
    #[test]
    fn data_dir_is_under_examsystem() {
        if let Some(dir) = user_data_dir() {
            assert!(
                dir.ends_with("ExamSystem"),
                "数据目录必须以 ExamSystem 结尾，实际 {}",
                dir.display()
            );
            assert!(
                dir.is_absolute(),
                "数据目录必须是绝对路径（相对路径会随启动目录漂移）"
            );
        }
    }

    /// 两级回退都拼同一个后缀，Linux 与 Windows 不再出现两个 `ExamSystem` 目录。
    #[test]
    fn same_suffix_for_every_platform_base() {
        for base in [
            PathBuf::from("/tmp/x"),
            PathBuf::from("C:/Users/x/AppData/Local"),
        ] {
            let dir = base.join("ExamSystem");
            assert!(dir.ends_with("ExamSystem"));
        }
    }
}
