//! 状态文件权限收紧（作答状态文件的第二道防线）。
//!
//! ## 为什么需要它
//!
//! `main.rs::save_state` 写出的 `{学号}-{场次}.json` 内容已经加密（`web-state`
//! 子密钥由 K_manifest 经 HKDF 派生），但这只保证**内容**不泄露，挡不住同机其他账户：
//!
//! - 替换或删除密文 → 回滚「已交卷」状态、抹掉答题记录、让服务重启后读到别的文件；
//! - 读走密文离线爆破（学生自设 PIN 往往很弱）。
//!
//! 而默认权限恰好是「继承来的、很宽」的：
//!
//! - Windows：新建文件继承父目录的 ACL，教师机常见的是 `Administrators` / `Users` /
//!   `Everyone` 三类受托人；同机其他账户能不能读，全看这台机器目录的既有权限，不能靠运气；
//! - Unix：mode 由 umask 决定，默认 0644 就是全机可读。
//!
//! 所以这里显式收紧：Unix 用 `chmod`（文件 0600 / 目录 0700）；Windows 设置
//! **受保护 DACL**（protected DACL，`SE_DACL_PROTECTED`）：切断从父目录的继承，
//! 只保留当前用户一条 `FILE_ALL_ACCESS`。目录上的那条 ACE 带
//! `SUB_CONTAINERS_AND_OBJECTS_INHERIT`，因此之后在该目录里**新建**的状态文件会
//! 自动继承同样的受限 ACL，不存在「先宽松后收紧」的窗口。
//!
//! 注意：继承只对收紧之后创建的子对象生效；收紧之前已经存在的子文件需要逐个调用
//! [`harden_file`]。
//!
//! ## Windows 为什么直接调 advapi32 而不是 `icacls.exe`
//!
//! `icacls` 的输出是**本地化文本**（中文 Windows 上全是中文），解析它等于把安全性
//! 绑到系统语言上；而且拉起子进程还多一条可被同名程序劫持的搜索路径。这里直接调
//! `SetEntriesInAclW` + `SetNamedSecurityInfoW`，失败信息取自 API 的 Windows 错误码，
//! 与系统语言无关，也不依赖任何外部程序。

use std::io;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(windows)]
use std::ffi::c_void;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use std::ptr;

#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, GetLastError, HANDLE, LocalFree};
#[cfg(windows)]
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, SET_ACCESS,
    SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
};
#[cfg(windows)]
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation, DACL_SECURITY_INFORMATION,
    EqualSid, GetAce, GetAclInformation, GetSecurityDescriptorControl, GetTokenInformation,
    NO_INHERITANCE, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    SE_DACL_PROTECTED, SUB_CONTAINERS_AND_OBJECTS_INHERIT, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// 收紧单个文件：Unix `chmod 0600`；Windows 设置只授予当前用户的受保护 DACL。
pub fn harden_file(path: &Path) -> io::Result<()> {
    harden_file_impl(path)
}

/// 收紧目录：Unix `chmod 0700`；Windows 受保护 DACL，并让子文件/子目录继承同样限制。
pub fn harden_dir(path: &Path) -> io::Result<()> {
    harden_dir_impl(path)
}

#[cfg(unix)]
fn harden_file_impl(path: &Path) -> io::Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(unix)]
fn harden_dir_impl(path: &Path) -> io::Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[cfg(windows)]
fn harden_file_impl(path: &Path) -> io::Result<()> {
    set_protected_dacl(path, false)
}

#[cfg(windows)]
fn harden_dir_impl(path: &Path) -> io::Result<()> {
    set_protected_dacl(path, true)
}

/// 其它平台（当前 CI 只跑 Linux / Windows）不做处理，保证 workspace 可编译。
#[cfg(not(any(unix, windows)))]
fn harden_file_impl(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn harden_dir_impl(_path: &Path) -> io::Result<()> {
    Ok(())
}

// ---------------------------------------------------------------------------
// Windows：受保护 DACL
// ---------------------------------------------------------------------------

/// 长路径阈值：超过它就先拼 `\\?\` 前缀。
///
/// `SetNamedSecurityInfoW` / `GetNamedSecurityInfoW` 走 Win32 的路径归一化，
/// 路径超过 `MAX_PATH`(260) 会被截断或直接失败（`ERROR_PATH_NOT_FOUND`）。
/// 官方建议给目录留余量，这里取 240：`\\?\` 前缀让 API 走不归一化的长路径分支。
/// 注意该前缀**禁用**路径解析，所以必须先绝对化——[`std::path::absolute`] 只做
/// 「当前目录 + 相对路径」的拼接，不访问磁盘、不解析 `..`，正好满足要求
/// （用 `canonicalize` 会要求文件存在，且会解析符号链接，语义不对）。
#[cfg(windows)]
const LONG_PATH_AT: usize = 240;

/// `ACCESS_ALLOWED_ACE_TYPE`（windows-sys 把它放在需要额外 feature 的模块里，
/// 这里按 Win32 定义写死；值来自 `ntifs.h`，不会变）。只在测试助手 `GetAce` 遍历里用。
#[cfg(windows)]
#[cfg_attr(not(test), allow(dead_code))]
const ACE_TYPE_ACCESS_ALLOWED: u8 = 0;

/// 把路径转成 `*W` 系列 API 需要的、以 NUL 结尾的宽字符序列。
///
/// 为什么相对路径必须先绝对化：`\\?\` 前缀会关闭路径归一化，相对路径加前缀后毫无意义；
/// 即使不加前缀，`SetNamedSecurityInfoW` 解析相对路径也依赖进程当前目录，容易被改名绕过。
#[cfg(windows)]
fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let absolute = std::path::absolute(path)?;
    let units: Vec<u16> = absolute.as_os_str().encode_wide().collect();
    let backslash = u16::from(b'\\');

    // 已经是 `\\?\` / `\\.\` 形式的不再重复加前缀。
    let already_prefixed = units.len() >= 4
        && units[0] == backslash
        && units[1] == backslash
        && (units[2] == u16::from(b'?') || units[2] == u16::from(b'.'))
        && units[3] == backslash;

    let mut out = if units.len() >= LONG_PATH_AT && !already_prefixed {
        let mut prefixed = Vec::with_capacity(units.len() + 8);
        if units.len() >= 2 && units[0] == backslash && units[1] == backslash {
            // UNC：`\\server\share\x` 的长路径形式是 `\\?\UNC\server\share\x`，
            // 不能直接在前面补 `\\?\`（那会变成 `\\?\\\server...`）。
            prefixed.extend("\\\\?\\UNC".encode_utf16());
            prefixed.extend_from_slice(&units[1..]);
        } else {
            prefixed.extend("\\\\?\\".encode_utf16());
            prefixed.extend_from_slice(&units);
        }
        prefixed
    } else {
        units
    };
    out.push(0);
    Ok(out)
}

/// 进程 token 句柄的 Drop guard：任何提前返回都不泄漏句柄。
#[cfg(windows)]
struct TokenHandle(HANDLE);

#[cfg(windows)]
impl Drop for TokenHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: 句柄由 OpenProcessToken 打开且只在这里关闭一次。
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// `SetEntriesInAclW` 产出的 ACL 由 `LocalAlloc` 分配，必须 `LocalFree`。
#[cfg(windows)]
struct LocalAcl(*mut ACL);

#[cfg(windows)]
impl Drop for LocalAcl {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: 指针来自 SetEntriesInAclW，所有权归调用方。
            unsafe { LocalFree(self.0.cast()) };
        }
    }
}

/// 失败信息统一带 API 名与 Windows 错误码：现场排查时能直接对照 `winerror.h`。
#[cfg(windows)]
fn api_error(api: &str, code: u32) -> io::Error {
    io::Error::other(format!("{api} 失败：Windows 错误码 {code}"))
}

/// 给返回 `BOOL` 的 API 用：错误码得从 `GetLastError` 取。
#[cfg(windows)]
fn last_error(api: &str) -> io::Error {
    // SAFETY: GetLastError 无参数、无副作用。
    let code = unsafe { GetLastError() };
    api_error(api, code)
}

/// 当前进程 token 里的用户 SID。
///
/// 必须连同查询缓冲区一起持有：`GetTokenInformation(TokenUser)` 返回的 `TOKEN_USER`
/// 以及其中的 `Sid` 都指向那块缓冲区内部，只保存 `Sid` 指针会变成悬垂指针。
#[cfg(windows)]
struct CurrentUserSid {
    /// 用 `usize` 承载而不是 `u8`：`TOKEN_USER` 里含指针，缓冲区必须指针对齐。
    buffer: Vec<usize>,
}

#[cfg(windows)]
impl CurrentUserSid {
    fn query() -> io::Result<Self> {
        let mut raw: HANDLE = ptr::null_mut();
        // SAFETY: 伪句柄 GetCurrentProcess() 不需要关闭；token 句柄交给 TokenHandle。
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) } == 0 {
            return Err(last_error("OpenProcessToken"));
        }
        let token = TokenHandle(raw);

        let mut needed: u32 = 0;
        // 先探长度：预期返回「缓冲区不足」，此时 needed 被写成实际需要的字节数。
        // SAFETY: 空缓冲区 + 长度 0 是 GetTokenInformation 规定的探测用法。
        let probed =
            unsafe { GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut needed) };
        if probed == 0 {
            // SAFETY: 同 last_error。
            let code = unsafe { GetLastError() };
            if code != windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER || needed == 0 {
                return Err(api_error("GetTokenInformation（探测长度）", code));
            }
        }
        if needed == 0 {
            return Err(io::Error::other(
                "GetTokenInformation 返回的 token 信息长度为 0",
            ));
        }

        let words = (needed as usize).div_ceil(std::mem::size_of::<usize>());
        let mut buffer = vec![0usize; words];
        // SAFETY: 缓冲区长度 >= needed 字节（按 usize 向上取整），对齐满足 TOKEN_USER。
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(last_error("GetTokenInformation"));
        }
        Ok(Self { buffer })
    }

    fn sid(&self) -> PSID {
        // SAFETY: buffer 由 GetTokenInformation 成功填充且仍然存活；
        // `usize` 缓冲区的对齐满足 TOKEN_USER 的要求。
        let user = unsafe { &*self.buffer.as_ptr().cast::<TOKEN_USER>() };
        user.User.Sid
    }
}

/// 给文件/目录设置「只有当前用户」的受保护 DACL。
///
/// 调用链：`OpenProcessToken` → `GetTokenInformation(TokenUser)` 取 SID →
/// `SetEntriesInAclW` 造新 ACL → `SetNamedSecurityInfoW` 写回（带
/// `PROTECTED_DACL_SECURITY_INFORMATION`）→ `LocalFree`。
#[cfg(windows)]
fn set_protected_dacl(path: &Path, is_dir: bool) -> io::Result<()> {
    let user = CurrentUserSid::query()?;

    // 显式 ACE：SET_ACCESS 表示替换该受托人的既有 ACE；继承位决定子对象是否跟着收紧。
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_ALL_ACCESS,
        grfAccessMode: SET_ACCESS,
        grfInheritance: if is_dir {
            SUB_CONTAINERS_AND_OBJECTS_INHERIT
        } else {
            NO_INHERITANCE
        },
        Trustee: TRUSTEE_W {
            pMultipleTrustee: ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: user.sid().cast::<u16>(),
        },
    };

    let mut new_acl: *mut ACL = ptr::null_mut();
    // oldacl = null：不与旧 ACL 合并，新 ACL 里就只有这一条 ACE，
    // 父目录继承来的 Administrators / Users / Everyone 就此消失。
    // SAFETY: entry 与 user 的缓冲区都存活到调用结束，SetEntriesInAclW 会复制 SID。
    let code = unsafe { SetEntriesInAclW(1, &entry, ptr::null(), &mut new_acl) };
    if code != ERROR_SUCCESS {
        return Err(api_error("SetEntriesInAclW", code));
    }
    let new_acl = LocalAcl(new_acl);

    let wide = wide_path(path)?;
    // PROTECTED_DACL_SECURITY_INFORMATION 置 SE_DACL_PROTECTED：之后不再从父目录继承 ACE。
    // 少了这一位，上级目录的 Everyone/Users 会通过继承重新出现在 ACL 里，这次收紧等于白做。
    // SAFETY: 宽字符路径以 NUL 结尾；new_acl 有效且由 LocalAcl 负责释放。
    let code = unsafe {
        SetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            new_acl.0,
            ptr::null(),
        )
    };
    if code != ERROR_SUCCESS {
        return Err(api_error("SetNamedSecurityInfoW", code));
    }
    Ok(())
}

/// 只读地打开路径的安全描述符，供测试断言真实 ACL。
#[cfg(windows)]
#[cfg_attr(not(test), allow(dead_code))]
struct Dacl {
    sd: PSECURITY_DESCRIPTOR,
    dacl: *mut ACL,
}

#[cfg(windows)]
#[cfg_attr(not(test), allow(dead_code))]
impl Dacl {
    fn open(path: &Path) -> io::Result<Self> {
        let wide = wide_path(path)?;
        let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
        let mut dacl: *mut ACL = ptr::null_mut();
        // SAFETY: 只请求 DACL；返回的 sd 由 LocalFree 释放（见 Drop），dacl 指向其内部。
        let code = unsafe {
            GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut dacl,
                ptr::null_mut(),
                &mut sd,
            )
        };
        if code != ERROR_SUCCESS {
            return Err(api_error("GetNamedSecurityInfoW", code));
        }
        Ok(Self { sd, dacl })
    }
}

#[cfg(windows)]
#[cfg_attr(not(test), allow(dead_code))]
impl Drop for Dacl {
    fn drop(&mut self) {
        if !self.sd.is_null() {
            // SAFETY: 安全描述符由 GetNamedSecurityInfoW 分配，必须 LocalFree。
            unsafe { LocalFree(self.sd) };
        }
    }
}

/// DACL 里的 ACE 条数（null DACL 视为 0 条）。
#[cfg(windows)]
#[cfg_attr(not(test), allow(dead_code))]
fn ace_count(dacl: *mut ACL) -> io::Result<usize> {
    if dacl.is_null() {
        return Ok(0);
    }
    let mut info = ACL_SIZE_INFORMATION::default();
    // SAFETY: dacl 非空且有效；info 的布局与长度与 AclSizeInformation 匹配。
    let ok = unsafe {
        GetAclInformation(
            dacl,
            (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    };
    if ok == 0 {
        return Err(last_error("GetAclInformation"));
    }
    Ok(info.AceCount as usize)
}

/// 测试助手：真实 DACL 的 ACE 条数。
#[cfg(windows)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn acl_ace_count(path: &Path) -> io::Result<usize> {
    let dacl = Dacl::open(path)?;
    ace_count(dacl.dacl)
}

/// 测试助手：DACL 是否只有「当前用户」这一个受托人。
///
/// 逐条遍历 ACE：类型必须是 ACCESS_ALLOWED，SID 必须等于当前进程 token 的用户 SID。
/// 空 DACL 与 null DACL 都返回 false：前者说明没有按预期写入（预期恰好一条显式 ACE），
/// 后者等于 Everyone 全权。
#[cfg(windows)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn acl_grants_only_current_user(path: &Path) -> io::Result<bool> {
    let user = CurrentUserSid::query()?;
    let dacl = Dacl::open(path)?;
    let count = ace_count(dacl.dacl)?;
    if count == 0 {
        return Ok(false);
    }
    for index in 0..count {
        let mut raw: *mut c_void = ptr::null_mut();
        // SAFETY: index < AceCount，dacl 仍由 dacl guard 持有。
        if unsafe { GetAce(dacl.dacl, index as u32, &mut raw) } == 0 {
            return Err(last_error("GetAce"));
        }
        // SAFETY: GetAce 返回的指针指向 DACL 内部的一条 ACE，长度由 AceSize 界定；
        // 先读 Header 判断类型，再按 ACCESS_ALLOWED_ACE 布局取 SID。
        let ace = unsafe { &*raw.cast::<ACCESS_ALLOWED_ACE>() };
        if ace.Header.AceType != ACE_TYPE_ACCESS_ALLOWED {
            return Ok(false);
        }
        // SID 就嵌在 ACE 尾部：从 SidStart 字段地址开始。
        let sid: PSID = ptr::addr_of!(ace.SidStart) as PSID;
        // SAFETY: sid 是有效 SID（ACE 内的 SidStart），user.sid() 来自活跃 token 缓冲区。
        if unsafe { EqualSid(sid, user.sid()) } == 0 {
            return Ok(false);
        }
    }
    Ok(true)
}

/// 测试助手：DACL 是否带 `SE_DACL_PROTECTED`（不再从父目录继承）。
#[cfg(windows)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn dacl_is_protected(path: &Path) -> io::Result<bool> {
    let dacl = Dacl::open(path)?;
    let mut control: u16 = 0;
    let mut revision: u32 = 0;
    // SAFETY: sd 来自成功的 GetNamedSecurityInfoW。
    if unsafe { GetSecurityDescriptorControl(dacl.sd, &mut control, &mut revision) } == 0 {
        return Err(last_error("GetSecurityDescriptorControl"));
    }
    Ok(control & SE_DACL_PROTECTED != 0)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试目录一律建在仓库 `target/` 下：AGENTS.md 明令禁止 `std::env::temp_dir()`
    /// （本机 MSYS 下它指向 `/tmp`，原生进程写不进去）。用例结束由 Drop 清理。
    struct TestDir {
        path: std::path::PathBuf,
    }

    impl TestDir {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static SEQ: AtomicUsize = AtomicUsize::new(0);
            let name = format!(
                "secure-fs-{tag}-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            );
            let path = target_root().join(name);
            std::fs::create_dir_all(&path).expect("创建测试目录失败");
            Self { path }
        }

        fn join(&self, name: &str) -> std::path::PathBuf {
            self.path.join(name)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// 仓库根下的 `target/`。
    ///
    /// `cargo test` 的工作目录是 crate 根（`crates/server`），这里用
    /// `CARGO_MANIFEST_DIR` 往上两级定位仓库根：不依赖进程当前目录，
    /// 即使测试被别处的工作目录拉起也不会写到意外位置。
    fn target_root() -> std::path::PathBuf {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("定位仓库根目录失败");
        let target = root.join("target");
        std::fs::create_dir_all(&target).expect("创建 target 目录失败");
        target
    }

    #[cfg(windows)]
    #[test]
    fn windows_harden_file_limits_dacl_to_current_user() {
        let dir = TestDir::new("file");
        let file = dir.join("state.json");
        std::fs::write(&file, b"ciphertext").unwrap();

        harden_file(&file).expect("harden_file 失败");

        assert!(
            acl_grants_only_current_user(&file).unwrap(),
            "收紧后 DACL 仍授予了其他受托人"
        );
        assert_eq!(
            acl_ace_count(&file).unwrap(),
            1,
            "受保护 DACL 应当只有当前用户一条 ACE"
        );
        assert!(
            dacl_is_protected(&file).unwrap(),
            "DACL 没有标记 SE_DACL_PROTECTED，仍会从父目录继承"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_harden_file_is_idempotent() {
        let dir = TestDir::new("idem");
        let file = dir.join("state.json");
        std::fs::write(&file, b"ciphertext").unwrap();

        harden_file(&file).expect("第一次 harden_file 失败");
        harden_file(&file).expect("第二次 harden_file 失败");

        assert_eq!(acl_ace_count(&file).unwrap(), 1, "重复收紧不应叠加 ACE");
        assert!(acl_grants_only_current_user(&file).unwrap());
        assert!(dacl_is_protected(&file).unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn windows_harden_dir_inherits_to_new_files() {
        let dir = TestDir::new("dir");
        let state_dir = dir.join("state");
        std::fs::create_dir(&state_dir).unwrap();

        harden_dir(&state_dir).expect("harden_dir 失败");

        // 故意不调用 harden_file：这里验证的是目录继承，而不是逐个文件收紧。
        let fresh = state_dir.join("student-state.json");
        std::fs::write(&fresh, b"ciphertext").unwrap();

        assert!(
            acl_grants_only_current_user(&fresh).unwrap(),
            "收紧目录里新建的文件没有继承受限 DACL"
        );
        assert_eq!(
            acl_ace_count(&fresh).unwrap(),
            1,
            "继承后应当只有当前用户一条 ACE"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_harden_missing_path_returns_err() {
        let dir = TestDir::new("missing");
        let missing = dir.join("nope.json");

        assert!(harden_file(&missing).is_err(), "不存在的文件应返回 Err");
        assert!(harden_dir(&missing).is_err(), "不存在的目录应返回 Err");
    }

    #[cfg(unix)]
    #[test]
    fn unix_harden_file_and_dir_modes() {
        // 显式再引入一次：不依赖 `use super::*` 把父模块的 trait 导入带进来。
        use std::os::unix::fs::PermissionsExt;

        let dir = TestDir::new("unix");
        let file = dir.join("state.json");
        std::fs::write(&file, b"ciphertext").unwrap();
        harden_file(&file).unwrap();
        let file_mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "状态文件应是 0600");

        let sub = dir.join("state");
        std::fs::create_dir(&sub).unwrap();
        harden_dir(&sub).unwrap();
        let dir_mode = std::fs::metadata(&sub).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "状态目录应是 0700");
    }
}
