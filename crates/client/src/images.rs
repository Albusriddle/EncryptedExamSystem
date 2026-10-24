//! 图片解码与纹理缓存。
//!
//! ## 为什么只认 data URI
//!
//! 考试包必须**离线自包含**：拷一个 exe 到考场就能考。所以图片以
//! `data:image/png;base64,…` 的形式内嵌在题干里，不读任何外部文件，
//! 也就不存在"路径穿越 / 临时文件残留"这类安全问题。
//!
//! ## 上限
//!
//! - 单张解码后 ≤ 4MB：防止一个超大 data URI 把内存打满；
//! - 解码后像素 ≤ 4096×4096：防止解压炸弹；
//! - 纹理缓存：128 MB 字节预算 + 32 张上限；
//! - **失败也缓存**：坏图不能每帧重试解码（一张坏掉的 4MB 图会持续吃 CPU）。

use std::cell::RefCell;
use std::collections::VecDeque;

use base64::Engine as _;
use eframe::egui;

/// 纹理缓存预算：按**字节**而不是张数算。
///
/// 单张 4096×4096 RGBA 纹理就是 64MB，按"32 张"限制最坏能吃到 2GB。
const CACHE_MAX_BYTES: usize = 128 * 1024 * 1024;
const CACHE_LIMIT: usize = 32;

/// 单张图片解码后的字节上限（base64 前）。
const MAX_BYTES: usize = 4 * 1024 * 1024;
/// 单边像素上限。
const MAX_PIXELS: u32 = 4096;
thread_local! {
    /// (键, 纹理, 估算字节数)
    static CACHE: RefCell<VecDeque<([u8; 16], egui::TextureHandle, usize)>> =
        const { RefCell::new(VecDeque::new()) };
    /// 解码失败的键：命中直接返回 None，不再重复解码
    static FAILED: RefCell<VecDeque<[u8; 16]>> = const { RefCell::new(VecDeque::new()) };
}

/// 失败缓存条数（坏图通常只有一两张，32 足够）。
const FAILED_LIMIT: usize = 32;

/// 取图片纹理：命中缓存直接返回，否则解码并上传。
///
/// **缓存键必须是 URI 字符串的哈希**：早期版本先 base64 解码再算键，
/// 命中缓存也要先付一次 ~2MB 的解码成本，滚动时每帧都卡。
pub fn texture_for_uri(ctx: &egui::Context, uri: &str) -> Option<egui::TextureHandle> {
    let key = key_of(uri);

    if let Some(hit) = CACHE.with(|c| {
        c.borrow()
            .iter()
            .find(|(k, _, _)| *k == key)
            .map(|(_, t, _)| t.clone())
    }) {
        return Some(hit);
    }

    if FAILED.with(|f| f.borrow().contains(&key)) {
        return None;
    }

    let bytes = match decode_data_uri(uri) {
        Some(b) => b,
        None => {
            remember_failure(key);
            return None;
        }
    };
    let tex = match decode_texture(ctx, &bytes, key) {
        Some(t) => t,
        None => {
            remember_failure(key);
            return None;
        }
    };
    let approx = tex.size()[0] * tex.size()[1] * 4;
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        c.push_back((key, tex.clone(), approx));
        let mut total: usize = c.iter().map(|(_, _, b)| *b).sum();
        while c.len() > CACHE_LIMIT || total > CACHE_MAX_BYTES {
            if let Some((_, _, freed)) = c.pop_front() {
                total = total.saturating_sub(freed);
            } else {
                break;
            }
        }
    });
    Some(tex)
}

fn remember_failure(key: [u8; 16]) {
    FAILED.with(|f| {
        let mut f = f.borrow_mut();
        if f.contains(&key) {
            return;
        }
        f.push_back(key);
        while f.len() > FAILED_LIMIT {
            f.pop_front();
        }
    });
}

/// 用 blake3 取 URI 的前 16 字节做键（避免 DefaultHasher 的碰撞串图风险）。
fn key_of(uri: &str) -> [u8; 16] {
    let h = blake3::hash(uri.as_bytes());
    let mut key = [0u8; 16];
    key.copy_from_slice(&h.as_bytes()[..16]);
    key
}

fn decode_texture(ctx: &egui::Context, bytes: &[u8], key: [u8; 16]) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(bytes).ok()?;
    if img.width() == 0 || img.height() == 0 {
        return None;
    }
    if img.width() > MAX_PIXELS || img.height() > MAX_PIXELS {
        return None;
    }
    let rgba = img.to_rgba8();
    let color = egui::ColorImage::from_rgba_unmultiplied(
        [img.width() as usize, img.height() as usize],
        rgba.as_raw(),
    );
    let name = format!(
        "img-{}",
        u64::from_le_bytes(key[..8].try_into().unwrap_or_default())
    );
    Some(ctx.load_texture(name, color, egui::TextureOptions::LINEAR))
}

/// 解析 `data:image/png;base64,…`；其它形式（http/文件路径）一律拒绝。
pub fn decode_data_uri(uri: &str) -> Option<Vec<u8>> {
    let rest = uri.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    if !meta.starts_with("image/") || !meta.contains(";base64") {
        return None;
    }
    // base64 体积约为原始数据的 4/3，先按字符串长度挡一道
    if data.len() > MAX_BYTES / 3 * 4 + 8 {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.trim())
        .ok()?;
    if bytes.len() > MAX_BYTES {
        return None;
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1×1 透明 PNG。
    const PNG_1PX: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

    #[test]
    fn decodes_png_data_uri() {
        let bytes = decode_data_uri(PNG_1PX).expect("应能解码");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn rejects_http_and_plain_paths() {
        assert!(decode_data_uri("https://example.com/a.png").is_none());
        assert!(decode_data_uri("images/a.png").is_none());
        assert!(decode_data_uri("data:text/plain;base64,AAAA").is_none());
    }

    #[test]
    fn rejects_broken_base64() {
        assert!(decode_data_uri("data:image/png;base64,@@@@").is_none());
    }

    #[test]
    fn decodes_to_texture() {
        let ctx = egui::Context::default();
        let tex = texture_for_uri(&ctx, PNG_1PX).expect("应能生成纹理");
        assert_eq!(tex.size(), [1, 1]);
    }

    #[test]
    fn broken_image_is_cached_as_failure() {
        let ctx = egui::Context::default();
        let uri = "data:image/png;base64,AAAA";
        assert!(texture_for_uri(&ctx, uri).is_none());
        // 第二次必须走失败缓存（不再解码），结果依旧是 None
        assert!(texture_for_uri(&ctx, uri).is_none());
    }

    #[test]
    fn broken_image_returns_none_instead_of_panic() {
        let ctx = egui::Context::default();
        // 合法的 base64，但不是图片
        let uri = "data:image/png;base64,AAAA";
        assert!(texture_for_uri(&ctx, uri).is_none());
    }
}
