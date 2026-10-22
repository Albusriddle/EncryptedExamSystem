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
}

/// AES-256-GCM 随机 nonce 长度（96 bit）。
pub const NONCE_LEN: usize = 12;

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
    use rand::RngCore;
    let mut key = [0u8; 32];
    rand::rng().fill_bytes(&mut key);
    hex::encode(key)
}

/// HKDF-SHA256 派生子密钥。
///
/// `ikm` 为主密钥，`info` 为用途标签（如 "payload" / "stub" / "manifest"）。
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

/// 学号入场密钥派生：用 Argon2id 拉慢离线爆破。
///
/// 用于"逐条试解密"的场次记录外层加密。每次尝试约 100ms，
/// 把 10^10 学号空间的爆破从几小时拉到数十年。
pub fn session_entry_key(student_id: u64, session_id: u32, salt: &[u8]) -> Zeroizing<[u8; 32]> {
    let mut input = Vec::with_capacity(12);
    input.extend_from_slice(&student_id.to_le_bytes());
    input.extend_from_slice(&session_id.to_le_bytes());
    // Argon2id 默认参数即可；对单条记录解密足够快，对爆破足够慢。
    let params = argon2::Params::new(8192, 3, 1, Some(32)).expect("argon2 params");
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut out = [0u8; 32];
    argon
        .hash_password_into(&input, salt, &mut out)
        .expect("argon2 hash");
    Zeroizing::new(out)
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
        let a = session_entry_key(2021001, 1, b"salt-123456789012");
        let b = session_entry_key(2021001, 1, b"salt-123456789012");
        assert_eq!(*a, *b);
        let c = session_entry_key(2021002, 1, b"salt-123456789012");
        assert_ne!(*a, *c);
    }
}
