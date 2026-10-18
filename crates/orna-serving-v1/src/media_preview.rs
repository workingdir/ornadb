//! Explicit, bounded raster-image preview decoding for stored Blobs.
//!
//! This entry point is intentionally separate from metadata projection and raw
//! media serving. It reads content only when called, through the Blob's pinned
//! resolver, and returns inert RGBA pixels rather than executable markup.

use std::io::Cursor;

use image::{ImageFormat, ImageReader, Limits as ImageLimits};
use orna_value_v1::Blob;

/// Maximum compressed representation bytes accepted for a preview.
pub const MAX_PREVIEW_INPUT_BYTES: u64 = 8 * 1024 * 1024;
/// Maximum width or height accepted by the pinned decoders.
pub const MAX_PREVIEW_DIMENSION: u32 = 4096;
/// Maximum number of decoded pixels accepted by the pinned decoders.
pub const MAX_PREVIEW_PIXELS: u64 = 4 * 1024 * 1024;
/// Maximum transient decoder allocation requested from the decoder.
pub const MAX_DECODER_ALLOCATION: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreviewError {
    UnsupportedMediaType,
    InputTooLarge,
    ContentUnavailable,
    MalformedOrOverLimit,
}

/// Decoded, non-executable raster pixels.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// Row-major RGBA8 bytes; length is exactly `width * height * 4`.
    pub rgba: Vec<u8>,
}

/// Decode a supported raster Blob after an explicit preview request.
///
/// Only PNG, JPEG, GIF and WebP are decoded. SVG, HTML, JavaScript and all
/// other declarations are rejected without reading content. No metadata-only
/// operation calls this function. The Blob reader enforces its own range and
/// content-integrity checks; this layer applies a stricter preview input cap.
pub fn decode_image_preview(blob: &Blob) -> Result<DecodedImage, PreviewError> {
    let format = match blob.media_type().split(';').next().unwrap_or("").trim() {
        "image/png" => ImageFormat::Png,
        "image/jpeg" => ImageFormat::Jpeg,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::WebP,
        _ => return Err(PreviewError::UnsupportedMediaType),
    };

    let length = blob.length();
    if length > MAX_PREVIEW_INPUT_BYTES {
        return Err(PreviewError::InputTooLarge);
    }
    let bytes = blob
        .read(0, length)
        .map_err(|_| PreviewError::ContentUnavailable)?;

    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = ImageLimits::default();
    limits.max_image_width = Some(MAX_PREVIEW_DIMENSION);
    limits.max_image_height = Some(MAX_PREVIEW_DIMENSION);
    limits.max_alloc = Some(MAX_DECODER_ALLOCATION);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|_| PreviewError::MalformedOrOverLimit)?;
    let width = decoded.width();
    let height = decoded.height();
    if width == 0
        || height == 0
        || width > MAX_PREVIEW_DIMENSION
        || height > MAX_PREVIEW_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_PREVIEW_PIXELS
    {
        return Err(PreviewError::MalformedOrOverLimit);
    }
    Ok(DecodedImage {
        width,
        height,
        rgba: decoded.into_rgba8().into_raw(),
    })
}
