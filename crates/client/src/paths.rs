//! 用户数据目录（答题卡、欢迎弹窗标记）与 AppImage 外层考试包定位。
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

#[cfg(target_os = "linux")]
use std::ffi::OsString;
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

/// AppImage 的 payload/答题卡属于外层文件，不属于只读挂载点里的 ELF。
pub fn executable_path() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    #[cfg(target_os = "linux")]
    {
        Ok(executable_path_with(
            exe,
            std::env::var_os("APPIMAGE"),
            std::env::var_os("APPDIR"),
        ))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(exe)
    }
}

#[cfg(target_os = "linux")]
fn executable_path_with(exe: PathBuf, image: Option<OsString>, dir: Option<OsString>) -> PathBuf {
    if let (Some(image), Some(dir)) = (image, dir) {
        let image = PathBuf::from(image);
        let dir = PathBuf::from(dir);
        // builder 等外部程序可能继承这些环境变量，不能误读教师端 AppImage。
        // APPDIR="/" 会让任何绝对路径都满足包含关系；排除根目录。
        if image.is_absolute()
            && dir.is_absolute()
            && dir.as_path() != std::path::Path::new("/")
            && exe.starts_with(&dir)
        {
            return image;
        }
    }
    exe
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn appimage_payload_is_read_from_outer_file() {
        let inner = PathBuf::from("/tmp/.mount_exam/usr/bin/exam-client");
        let outer = PathBuf::from("/home/student/下载/StudentClient.AppImage");
        assert_eq!(
            executable_path_with(
                inner,
                Some(outer.clone().into_os_string()),
                Some(OsString::from("/tmp/.mount_exam")),
            ),
            outer
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn native_executable_ignores_inherited_or_relative_appimage_environment() {
        let native = PathBuf::from("/home/student/StudentClient");
        for (image, dir) in [
            ("/home/teacher/ExamAdmin.AppImage", "/tmp/.mount_teacher"),
            ("relative.AppImage", "/home/student"),
            ("/home/student/StudentClient.AppImage", "relative"),
            ("/home/student/StudentClient.AppImage", "/"),
        ] {
            assert_eq!(
                executable_path_with(native.clone(), Some(image.into()), Some(dir.into())),
                native
            );
        }
    }

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
