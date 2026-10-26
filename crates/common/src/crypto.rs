//! 加密与密钥派生。两端（builder / client）共用，务必保持一致。

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

/// 加密错误。
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("AEAD 加密失败")]
    Encrypt,
    #[error("AEAD 解密失败（密钥错误或数据被篡改）")]
    Decrypt,
    #[error("密钥长度错误: 期望 32 字节, 实际 {0}")]
    BadKeyLen(usize),
    #[error("hex 解码失败: {0}")]
    Hex(#[from] hex::FromHexError),
    #[error("KDF 参数非法: {0}")]
    Kdf(String),
}

/// AES-256-GCM 随机 nonce 长度（96 bit）。
pub const NONCE_LEN: usize = 12;

/// 信封标签长度（字节）。
pub const TAG_LEN: usize = 8;

/// Argon2id 参数。
///
/// **参数随包一起存储**（写在 header 里），因此将来调参不会让旧包失效，
/// 单元测试也可以用廉价参数跑得飞快。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KdfParams {
    /// 内存成本（KiB）
    pub m_cost: u32,
    /// 迭代次数
    pub t_cost: u32,
    /// 并行度
    pub p_cost: u32,
}

impl Default for KdfParams {
    /// 生产参数：64 MiB / 3 轮 / 1 路。
    ///
    /// **数值来自本机实测（2026-10-22，20 线程 CPU）**：
    ///
    /// | 参数 | 单次 | 8 线程 10^5 | 8 线程 10^6 |
    /// |---|---|---|---|
    /// | 19 MiB / t=2（旧值） | 18.2 ms | **3.8 分钟** | 0.6 小时 |
    /// | 64 MiB / t=3（现值） | 92.2 ms | **19.2 分钟** | 3.2 小时 |
    /// | 256 MiB / t=3 | 392.6 ms | 81.8 分钟 | 13.6 小时 |
    ///
    /// 旧值把枚举成本低估了一个数量级（文档曾误称 67 分钟，实测仅 3.8 分钟）。
    /// 现值的代价：1000 名学生的包在 8 线程下约 12 秒构建，可接受。
    ///
    /// 注：学号是低熵数据，离线可验证就意味着最终可枚举——这里的参数只是
    /// 把成本抬高，不是密码学保证。
    fn default() -> Self {
        Self {
            m_cost: 65536,
            t_cost: 3,
            p_cost: 1,
        }
    }
}

impl KdfParams {
    /// `m_cost` 下限（KiB）。低于此值枚举成本过低。
    pub const MIN_M_COST: u32 = 8;
    /// `m_cost` 上限（KiB）。1 GiB。
    ///
    /// 必须有上限：参数来自**包内的 header**，而 argon2 crate 的
    /// `MAX_M_COST` 就是 `u32::MAX`（等于不设防）。一个 m_cost=4e9 的包
    /// 会让客户端尝试分配约 4 TB，直接 abort，且没有任何可读错误。
    pub const MAX_M_COST: u32 = 1_048_576;
    pub const MIN_T_COST: u32 = 1;
    pub const MAX_T_COST: u32 = 16;
    pub const MIN_P_COST: u32 = 1;
    pub const MAX_P_COST: u32 = 8;

    /// 校验参数范围。用于解析不可信输入（包内 header）。
    pub fn validate(&self) -> Result<(), CryptoError> {
        if self.m_cost < Self::MIN_M_COST || self.m_cost > Self::MAX_M_COST {
            return Err(CryptoError::Kdf(format!(
                "m_cost={} 超出允许范围 [{}, {}]",
                self.m_cost,
                Self::MIN_M_COST,
                Self::MAX_M_COST
            )));
        }
        if self.t_cost < Self::MIN_T_COST || self.t_cost > Self::MAX_T_COST {
            return Err(CryptoError::Kdf(format!(
                "t_cost={} 超出允许范围 [{}, {}]",
                self.t_cost,
                Self::MIN_T_COST,
                Self::MAX_T_COST
            )));
        }
        if self.p_cost < Self::MIN_P_COST || self.p_cost > Self::MAX_P_COST {
            return Err(CryptoError::Kdf(format!(
                "p_cost={} 超出允许范围 [{}, {}]",
                self.p_cost,
                Self::MIN_P_COST,
                Self::MAX_P_COST
            )));
        }
        Ok(())
    }

    /// 生产强度下限：低于此值，名单可在秒级被枚举完。
    ///
    /// 与管理端 `models.py::KdfParams` 的下限保持一致。
    pub const PROD_MIN_M_COST: u32 = 65_536;
    pub const PROD_MIN_T_COST: u32 = 3;
    /// 生产强度的 salt 最小字节数（管理端同值）。
    pub const PROD_MIN_SALT_LEN: usize = 16;

    /// 参数是否达到生产强度。
    ///
    /// 与 [`validate`](Self::validate) 的区别：
    /// - `validate` 只管"能不能安全地算"（防止 TB 级内存分配）；
    /// - 本函数管"够不够强"——m=8/t=1 能算，但枚举成本几乎为零。
    ///
    /// 为什么要单独有这一层：管理端有严格下限，但 `--manifest` 是文档化的
    /// 官方用法，手工写清单会完全绕过 Python 校验。实测 m=8/t=1 可以直接
    /// 打包成功，而这样的包枚举 10^5 个学号只需几秒。
    pub fn meets_production_strength(&self) -> bool {
        self.m_cost >= Self::PROD_MIN_M_COST && self.t_cost >= Self::PROD_MIN_T_COST
    }

    /// 仅供测试使用的最低成本参数。
    pub fn cheap_for_tests() -> Self {
        Self {
            m_cost: 8,
            t_cost: 1,
            p_cost: 1,
        }
    }

    fn to_argon2(self) -> Result<argon2::Params, CryptoError> {
        // 每次派生都先校验：无论参数来自 manifest 还是包内 header，
        // 都不可能绕开上限去申请巨量内存。
        self.validate()?;
        argon2::Params::new(self.m_cost, self.t_cost, self.p_cost, Some(32))
            .map_err(|e| CryptoError::Kdf(e.to_string()))
    }
}

/// 从 64 位十六进制字符串解析主密钥（32 字节）。
pub fn master_from_hex(s: &str) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    let bytes = hex::decode(s.trim())?;
    if bytes.len() != 32 {
        return Err(CryptoError::BadKeyLen(bytes.len()));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Ok(Zeroizing::new(key))
}

/// 生成随机主密钥，返回 64 位 hex 字符串。
pub fn generate_master_hex() -> String {
    hex::encode(*random_key())
}

/// 生成 32 字节随机密钥。
pub fn random_key() -> Zeroizing<[u8; 32]> {
    use rand::RngCore;
    let mut key = [0u8; 32];
    rand::rng().fill_bytes(&mut key);
    Zeroizing::new(key)
}

/// HKDF-SHA256 派生子密钥。
///
/// `ikm` 为主密钥，`info` 为用途标签（如 "payload" / "bank" / "manifest"）。
pub fn hkdf_derive(ikm: &[u8; 32], salt: &[u8], info: &str) -> Zeroizing<[u8; 32]> {
    let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
    let mut okm = [0u8; 32];
    hk.expand(info.as_bytes(), &mut okm)
        .expect("HKDF expand 32 bytes 永不失败");
    Zeroizing::new(okm)
}

/// AES-256-GCM 加密。返回 `nonce || ciphertext`。
pub fn encrypt(key: &[u8; 32], plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
    use rand::RngCore;
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| CryptoError::BadKeyLen(key.len()))?;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce = &Nonce::from(nonce_bytes);
    let ct = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Encrypt)?;
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// AES-256-GCM 解密 `nonce || ciphertext`。
pub fn decrypt(key: &[u8; 32], data: &[u8], aad: &[u8]) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    if data.len() < NONCE_LEN {
        return Err(CryptoError::Decrypt);
    }
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| CryptoError::BadKeyLen(key.len()))?;
    let mut nonce_arr = [0u8; NONCE_LEN];
    nonce_arr.copy_from_slice(&data[..NONCE_LEN]);
    let nonce = &Nonce::from(nonce_arr);
    let pt = cipher
        .decrypt(
            nonce,
            Payload {
                msg: &data[NONCE_LEN..],
                aad,
            },
        )
        .map_err(|_| CryptoError::Decrypt)?;
    Ok(Zeroizing::new(pt))
}

/// 学号入场密钥派生。
///
/// 用 Argon2id 把"每个候选学号一次尝试"的成本抬到约 40ms，
/// 从而把离线枚举名单的成本从"瞬间"抬到"数十分钟以上"。
pub fn session_entry_key(
    student_id: u64,
    session_id: u32,
    salt: &[u8],
    kdf: KdfParams,
    pin: Option<&str>,
) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    let mut input = Vec::with_capacity(24);
    input.extend_from_slice(&student_id.to_le_bytes());
    input.extend_from_slice(&session_id.to_le_bytes());
    // 旧包（无 PIN）绝不加任何前缀，保证 v3 包仍能解锁。
    if let Some(pin) = pin {
        input.extend_from_slice(b"pin:");
        input.extend_from_slice(pin.as_bytes());
    }
    let params = kdf.to_argon2()?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut out = [0u8; 32];
    argon
        .hash_password_into(&input, salt, &mut out)
        .map_err(|e| CryptoError::Kdf(e.to_string()))?;
    Ok(Zeroizing::new(out))
}

/// 由入场密钥算出信封标签。
///
/// 标签让客户端 **只算一次 Argon2** 就能定位自己的信封，
/// 而不是对全部信封逐个尝试（1000 人时那是 40 秒 vs 40 毫秒的差别）。
///
/// 标签本身不额外泄漏信息：攻击者想用标签反查学号，
/// 仍然必须对每个候选学号跑一次 Argon2。
pub fn envelope_tag(key: &[u8; 32]) -> [u8; TAG_LEN] {
    let h = blake3::hash(key);
    let mut tag = [0u8; TAG_LEN];
    tag.copy_from_slice(&h.as_bytes()[..TAG_LEN]);
    tag
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_hex_roundtrip() {
        let hex_key = generate_master_hex();
        assert_eq!(hex_key.len(), 64);
        let key = master_from_hex(&hex_key).unwrap();
        assert_eq!(hex::encode(*key), hex_key);
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key = [42u8; 32];
        let aad = b"test-aad";
        let msg = "保密考试系统 payload".as_bytes();
        let ct = encrypt(&key, msg, aad).unwrap();
        assert_ne!(&ct[NONCE_LEN..], msg);
        let pt = decrypt(&key, &ct, aad).unwrap();
        assert_eq!(&*pt, msg);
    }

    #[test]
    fn decrypt_with_wrong_key_fails() {
        let key = [1u8; 32];
        let wrong = [2u8; 32];
        let ct = encrypt(&key, b"secret", b"").unwrap();
        assert!(decrypt(&wrong, &ct, b"").is_err());
    }

    #[test]
    fn decrypt_with_wrong_aad_fails() {
        let key = [1u8; 32];
        let ct = encrypt(&key, b"secret", b"aad1").unwrap();
        assert!(decrypt(&key, &ct, b"aad2").is_err());
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let key = [7u8; 32];
        let mut ct = encrypt(&key, b"secret", b"").unwrap();
        let last = ct.len() - 1;
        ct[last] ^= 1;
        assert!(decrypt(&key, &ct, b"").is_err());
    }

    #[test]
    fn hkdf_is_deterministic() {
        let ikm = [9u8; 32];
        let a = hkdf_derive(&ikm, b"salt", "payload");
        let b = hkdf_derive(&ikm, b"salt", "payload");
        assert_eq!(*a, *b);
        let c = hkdf_derive(&ikm, b"salt", "stub");
        assert_ne!(*a, *c);
    }

    #[test]
    fn session_entry_key_deterministic() {
        let kdf = KdfParams::cheap_for_tests();
        let a = session_entry_key(2021001, 1, b"salt-123456789012", kdf, None).unwrap();
        let b = session_entry_key(2021001, 1, b"salt-123456789012", kdf, None).unwrap();
        assert_eq!(*a, *b);
        let c = session_entry_key(2021002, 1, b"salt-123456789012", kdf, None).unwrap();
        assert_ne!(*a, *c);
        let d = session_entry_key(2021001, 2, b"salt-123456789012", kdf, None).unwrap();
        assert_ne!(*a, *d);
        let e = session_entry_key(2021001, 1, b"salt-123456789012", kdf, Some("123456")).unwrap();
        assert_ne!(*a, *e);
        let f = session_entry_key(2021001, 1, b"salt-123456789012", kdf, Some("654321")).unwrap();
        assert_ne!(*e, *f);
    }

    /// 生产参数必须明显强于测试参数，防止有人误把 cheap 参数用于发布。
    #[test]
    fn production_kdf_params_are_strong() {
        let p = KdfParams::default();
        assert!(p.m_cost >= 65536, "内存成本过低，枚举会变得廉价");
        assert!(p.t_cost >= 3, "迭代次数过低");
    }

    /// 参数范围必须拦住极端值。
    ///
    /// 真实风险：header 里的 kdf 参数完全由包决定，而 argon2 crate
    /// 的 MAX_M_COST 就是 u32::MAX。没有上限时，一个手改的包能让
    /// 客户端尝试分配数 TB 内存并 abort。
    #[test]
    fn kdf_params_bounds_are_enforced() {
        let ok = KdfParams::default();
        assert!(ok.validate().is_ok(), "生产默认值必须通过");

        for bad in [
            KdfParams { m_cost: 0, ..ok },
            KdfParams {
                m_cost: 4_000_000_000,
                ..ok
            },
            KdfParams {
                m_cost: KdfParams::MAX_M_COST + 1,
                ..ok
            },
            KdfParams { t_cost: 0, ..ok },
            KdfParams { t_cost: 999, ..ok },
            KdfParams { p_cost: 0, ..ok },
            KdfParams { p_cost: 99, ..ok },
        ] {
            assert!(bad.validate().is_err(), "应当拒绝越界参数: {bad:?}");
        }
        assert!(KdfParams::cheap_for_tests().validate().is_ok());
    }

    /// 生产强度判定：默认参数通过，廉价参数不通过。
    #[test]
    fn production_strength_threshold() {
        assert!(KdfParams::default().meets_production_strength());
        assert!(!KdfParams::cheap_for_tests().meets_production_strength());
        assert!(
            !KdfParams {
                m_cost: 8,
                t_cost: 3,
                p_cost: 1
            }
            .meets_production_strength()
        );
        assert!(
            !KdfParams {
                m_cost: 65536,
                t_cost: 1,
                p_cost: 1
            }
            .meets_production_strength()
        );
        assert!(
            KdfParams {
                m_cost: 65536,
                t_cost: 3,
                p_cost: 1
            }
            .meets_production_strength()
        );
    }

    /// 越界参数在派生阶段就被挡住，不会真的去分配内存。
    #[test]
    fn oversized_m_cost_fails_before_allocating() {
        let bad = KdfParams {
            m_cost: u32::MAX,
            ..KdfParams::default()
        };
        let err = session_entry_key(1, 1, b"0123456789abcdef", bad, None).unwrap_err();
        assert!(
            matches!(err, CryptoError::Kdf(_)),
            "应返回 KDF 参数错误: {err}"
        );
    }

    #[test]
    fn envelope_tag_is_stable_and_short() {
        let k = [3u8; 32];
        assert_eq!(envelope_tag(&k), envelope_tag(&k));
        assert_eq!(envelope_tag(&k).len(), TAG_LEN);
        assert_ne!(envelope_tag(&k), envelope_tag(&[4u8; 32]));
    }
}
