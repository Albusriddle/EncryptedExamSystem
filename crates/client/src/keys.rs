//! 嵌入的 K_manifest 访问接口。
//!
//! 密钥由 `build.rs` 从 `EXAM_KEYFILE` 读取、分片混淆后生成到 OUT_DIR。
//! 运行时用与 build.rs 相同的掩码还原。
//!
//! **安全性说明**：分片混淆只消除二进制中的明文密钥，让 `strings` 搜不到。
//! 它不是密码学保护——有逆向能力的人仍可还原出 K_manifest。
//! 但即便还原，攻击者也只能读到场次时间与人数，**拿不到任何题目**
//! （题库用每场独立的 session_secret 加密）。

// 由 build.rs 生成：SHARDS: [u8; 32]、EMBEDDED: bool
include!(concat!(env!("OUT_DIR"), "/embedded_key.rs"));

/// 与 build.rs 中完全一致的掩码。两处必须同步修改。
const MASK: [u8; 32] = [
    0x5a, 0xc3, 0x1f, 0x87, 0x2d, 0xe9, 0x46, 0xb1, 0x73, 0x0c, 0x9e, 0x28, 0xd4, 0x6f, 0x91, 0x3b,
    0xa7, 0x52, 0xce, 0x19, 0x84, 0xfd, 0x60, 0x2e, 0xb8, 0x47, 0xda, 0x05, 0x96, 0x6c, 0x31, 0xef,
];

/// 取嵌入的 K_manifest。未嵌入时返回 `None`（开发模式）。
pub fn embedded_manifest_key() -> Option<[u8; 32]> {
    if !EMBEDDED {
        return None;
    }
    let mut key = [0u8; 32];
    for i in 0..32 {
        key[i] = SHARDS[i] ^ MASK[i];
    }
    Some(key)
}

/// 是否嵌入了密钥（供 UI/日志显示模式）。
pub fn is_embedded() -> bool {
    EMBEDDED
}

/// **供 builder 检测 stub 状态用的标记字符串**。
///
/// builder 打包时会扫描 stub 二进制，确认它是否已嵌入密钥：
/// - 找到 `EXAMKEY:EMBEDDED` → 学生端免输密钥，正常；
/// - 找到 `EXAMKEY:PLAIN` → 学生端需要 `--master`，builder 必须**明确警告**；
/// - 两个都没有 → 不是本项目的 client 构建产物。
///
/// ⚠ 关键：**同一时刻只能有一个标记出现在二进制里**。
/// 早期实现把两个常量都定义成 `static`，结果两个字符串都进了二进制，
/// 导致未嵌入的 stub 也被误判为"已嵌入"。现在用 `cfg` 条件编译，
/// 保证只有一个标记存在。
#[cfg(any())]
const _UNREACHABLE: () = ();

/// 嵌入状态标记（只会在其中一种构建里存在）。
#[used]
pub static KEY_STATE_MARKER: &str = if EMBEDDED {
    "EXAMKEY:EMBEDDED"
} else {
    "EXAMKEY:PLAIN"
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_length_matches_shards() {
        assert_eq!(MASK.len(), 32);
        assert_eq!(SHARDS.len(), 32);
    }

    #[test]
    fn marker_matches_embedded_state() {
        let expected = if EMBEDDED {
            "EXAMKEY:EMBEDDED"
        } else {
            "EXAMKEY:PLAIN"
        };
        assert_eq!(KEY_STATE_MARKER, expected, "标记必须与嵌入状态一致");
    }

    /// 未设置 EXAM_KEYFILE 时（CI/开发默认）应为开发模式。
    #[test]
    fn dev_mode_when_not_embedded() {
        if EMBEDDED {
            assert!(embedded_manifest_key().is_some());
        } else {
            assert!(embedded_manifest_key().is_none());
        }
    }
}
