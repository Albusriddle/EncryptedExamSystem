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
//! - 解码**前**就按 `Limits` 限死像素 ≤ 4096×4096、解码内存 ≤ 64MB：防解压炸弹；
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
    let img = decode_with_limits(bytes).ok()?;
    if img.width() == 0 || img.height() == 0 {
        return None;
    }
    // 限制已在解码前生效（见 `decode_with_limits`）；这里再挡一次是双保险：
    // 万一某个格式的 decoder 不支持限制，也绝不能把超大图上传成纹理。
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

/// 解码后的内存上限：4096×4096 RGBA 约 64 MiB，留一点余量给解码中间缓冲。
const MAX_DECODE_ALLOC: u64 = 64 * 1024 * 1024;

/// 在**解码前**用 `image` 的 `Limits` 限死尺寸与内存。
///
/// 为什么不能「先 `load_from_memory` 再查宽高」：那样一张 4 MB 的 PNG 可以解压成
/// 几百 MB 才被拒（`image` 默认 `max_alloc = 512 MiB`），学生机上就是一次内存尖峰，
/// 极端情况直接 OOM。用 `ImageReader::with_guessed_format` + `limits`，
/// 解码器读 IHDR 时就按 `max_image_width/height` 拒绝，`reserve()` 再按字节卡一道，
/// 两种超限都会返回 `ImageError::Limits`。
fn decode_with_limits(bytes: &[u8]) -> Result<image::DynamicImage, image::ImageError> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_PIXELS);
    limits.max_image_height = Some(MAX_PIXELS);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    reader.decode()
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

    /// 构造一个只含 IHDR 的 PNG：只声明尺寸，不带像素数据。
    ///
    /// CRC 必须算对——`png` 解码器会校验块 CRC，CRC 错会先报 Format 错，
    /// 那样就测不出「按尺寸限制拒绝」这条路径了。
    fn png_with_declared_size(width: u32, height: u32) -> Vec<u8> {
        fn crc32(data: &[u8]) -> u32 {
            let mut crc = 0xffff_ffffu32;
            for &b in data {
                crc ^= b as u32;
                for _ in 0..8 {
                    let mask = (crc & 1).wrapping_neg();
                    crc = (crc >> 1) ^ (0xedb8_8320 & mask);
                }
            }
            !crc
        }
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        // bit depth 8、color type 6（RGBA）、无压缩/过滤/交错
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);

        let mut chunk = b"IHDR".to_vec();
        chunk.extend_from_slice(&ihdr);

        let mut out = Vec::new();
        out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        out.extend_from_slice(&(ihdr.len() as u32).to_be_bytes());
        out.extend_from_slice(&chunk);
        out.extend_from_slice(&crc32(&chunk).to_be_bytes());
        out
    }

    /// 声明尺寸超过 4096 的 PNG 必须在**解码像素之前**被限制拒绝。
    ///
    /// 为什么重要：旧实现先 `load_from_memory` 再查宽高，解压炸弹能先吃掉几百 MB；
    /// 现在靠 `Limits` 在读 IHDR 时就报 `ImageError::Limits`。
    #[test]
    fn oversized_png_is_rejected_by_limits_before_decoding() {
        let png = png_with_declared_size(10_000, 10_000);
        match decode_with_limits(&png) {
            Err(image::ImageError::Limits(_)) => {}
            other => panic!("应在分配像素前被尺寸限制拒绝，实际：{other:?}"),
        }
    }

    /// 声明尺寸在限制内的 PNG 不会被「限制」误伤（此处仍因缺像素数据而失败，
    /// 但错误不能是 Limits —— 那是我们自己的检查，不是文件本身的问题）。
    #[test]
    fn small_png_is_not_rejected_by_limits() {
        let png = png_with_declared_size(64, 64);
        if let Err(image::ImageError::Limits(e)) = decode_with_limits(&png) {
            panic!("64×64 不应触发限制：{e:?}");
        }
    }

    /// 通过 `texture_for_uri` 的路径也要拒绝（并且失败会被缓存，不会每帧重试）。
    #[test]
    fn oversized_png_uri_returns_none() {
        let png = png_with_declared_size(9_000, 9_000);
        let uri = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&png)
        );
        let ctx = egui::Context::default();
        assert!(texture_for_uri(&ctx, &uri).is_none());
        assert!(texture_for_uri(&ctx, &uri).is_none(), "第二次应走失败缓存");
    }
}
