//! Explicit, bounded media inspection for stored Blobs.
//! Reads occur only on explicit request through the selected Blob's pinned resolver.
use image::{ImageFormat, ImageReader, Limits as ImageLimits};
use std::io::Cursor;
pub const MAX_PREVIEW_INPUT_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_PREVIEW_DIMENSION: u32 = 4096;
pub const MAX_PREVIEW_PIXELS: u64 = 4 * 1024 * 1024;
pub const MAX_DECODER_ALLOCATION: u64 = 16 * 1024 * 1024;
pub const MAX_WAVE_INPUT_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_WAVE_CHANNELS: u16 = 32;
pub const MAX_WAVE_SAMPLE_RATE: u32 = 384_000;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreviewError {
    UnsupportedMediaType,
    ContentUnavailable,
    /// The bytes do not conform to the declared format (ORNA-MEDIA-002).
    Malformed,
    /// Well-formed content that exceeds a pinned decoder bound (ORNA-MEDIA-005).
    ResourceLimit,
}
/// The image formats the pinned decoder reads for a declared essence. `None` refuses the
/// declaration before any byte is read.
#[must_use]
pub fn image_format_for(essence: &str) -> Option<ImageFormat> {
    match essence {
        "image/png" => Some(ImageFormat::Png),
        "image/jpeg" => Some(ImageFormat::Jpeg),
        "image/gif" => Some(ImageFormat::Gif),
        "image/webp" => Some(ImageFormat::WebP),
        _ => None,
    }
}

/// The declared essences that name PCM WAVE. Their bytes must carry the RIFF/WAVE magic.
#[must_use]
pub fn is_wave_essence(essence: &str) -> bool {
    matches!(
        essence,
        "audio/wav" | "audio/x-wav" | "audio/wave" | "audio/vnd.wave"
    )
}

/// The properties of a fully decoded image. The pixel buffer is dropped inside
/// [`decode_image_preview`]; no RGBA copy is made.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImagePreview {
    pub width: u32,
    pub height: u32,
}

/// Fully decodes `bytes` as `format` and reports its dimensions. `bytes` is moved into the
/// decoder, not copied. Peak allocation is `bytes` plus at most [`MAX_DECODER_ALLOCATION`].
pub fn decode_image_preview(
    format: ImageFormat,
    bytes: Vec<u8>,
) -> Result<ImagePreview, PreviewError> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = ImageLimits::default();
    limits.max_image_width = Some(MAX_PREVIEW_DIMENSION);
    limits.max_image_height = Some(MAX_PREVIEW_DIMENSION);
    limits.max_alloc = Some(MAX_DECODER_ALLOCATION);
    reader.limits(limits);
    let decoded = reader.decode().map_err(preview_error)?;
    let (width, height) = (decoded.width(), decoded.height());
    drop(decoded);
    if width == 0 || height == 0 {
        return Err(PreviewError::Malformed);
    }
    if width > MAX_PREVIEW_DIMENSION
        || height > MAX_PREVIEW_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_PREVIEW_PIXELS
    {
        return Err(PreviewError::ResourceLimit);
    }
    Ok(ImagePreview { width, height })
}

/// Classifies a decoder failure. A truncated stream surfaces as an I/O `UnexpectedEof`
/// and is malformed content; other I/O failures are unavailable content, not a verdict on
/// the bytes. `Unsupported` names a valid feature the pinned decoder declines (not malformed).
fn preview_error(error: image::ImageError) -> PreviewError {
    match error {
        image::ImageError::Limits(_) => PreviewError::ResourceLimit,
        image::ImageError::Unsupported(_) => PreviewError::UnsupportedMediaType,
        image::ImageError::IoError(io) if io.kind() == std::io::ErrorKind::UnexpectedEof => {
            PreviewError::Malformed
        }
        image::ImageError::IoError(_) => PreviewError::ContentUnavailable,
        image::ImageError::Decoding(_)
        | image::ImageError::Encoding(_)
        | image::ImageError::Parameter(_) => PreviewError::Malformed,
    }
}

#[cfg(test)]
mod preview_error_tests {
    use super::{PreviewError, preview_error};
    use image::error::{ImageFormatHint, UnsupportedError, UnsupportedErrorKind};

    #[test]
    fn unsupported_decoder_feature_is_unsupported_media_not_malformed() {
        let error = image::ImageError::Unsupported(UnsupportedError::from_format_and_kind(
            ImageFormatHint::Unknown,
            UnsupportedErrorKind::GenericFeature("progressive jpeg".to_owned()),
        ));
        assert_eq!(preview_error(error), PreviewError::UnsupportedMediaType);
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaveInspectionError {
    UnsupportedFormat,
    MalformedContent,
    ResourceLimit,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PcmWaveProperties {
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    pub frame_count: u64,
    pub duration_nanos: u64,
    pub embedded_title: Option<&'static str>,
}
/// Inspects owned PCM WAVE bytes. The caller has already admitted an `audio/*` WAVE
/// essence (see [`is_wave_essence`]); bytes that lack the RIFF/WAVE magic are malformed,
/// because the declaration never substitutes for byte validation (ORNA-MEDIA-002).
pub fn inspect_pcm_wave(bytes: Vec<u8>) -> Result<PcmWaveProperties, WaveInspectionError> {
    parse_pcm_wave(&bytes)
}
fn parse_pcm_wave(bytes: &[u8]) -> Result<PcmWaveProperties, WaveInspectionError> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(WaveInspectionError::MalformedContent);
    }
    let riff_size = u32::from_le_bytes(bytes[4..8].try_into().expect("four-byte field")) as usize;
    let riff_end = riff_size
        .checked_add(8)
        .ok_or(WaveInspectionError::MalformedContent)?;
    if riff_end < 12 || riff_end != bytes.len() {
        return Err(WaveInspectionError::MalformedContent);
    }
    let mut cursor = 12usize;
    let mut fmt = None;
    let mut data_len = None;
    while cursor < riff_end {
        let header_end = cursor
            .checked_add(8)
            .ok_or(WaveInspectionError::MalformedContent)?;
        if header_end > riff_end {
            return Err(WaveInspectionError::MalformedContent);
        }
        let id = &bytes[cursor..cursor + 4];
        let chunk_len = u32::from_le_bytes(
            bytes[cursor + 4..header_end]
                .try_into()
                .expect("four-byte field"),
        ) as usize;
        let payload_end = header_end
            .checked_add(chunk_len)
            .ok_or(WaveInspectionError::MalformedContent)?;
        let padded_end = payload_end
            .checked_add(chunk_len & 1)
            .ok_or(WaveInspectionError::MalformedContent)?;
        if payload_end > riff_end || padded_end > riff_end {
            return Err(WaveInspectionError::MalformedContent);
        }
        if id == b"fmt " {
            if fmt.is_some() || chunk_len < 16 {
                return Err(WaveInspectionError::MalformedContent);
            }
            fmt = Some(&bytes[header_end..payload_end]);
        } else if id == b"data" && data_len.replace(chunk_len).is_some() {
            return Err(WaveInspectionError::MalformedContent);
        }
        cursor = padded_end;
    }
    let fmt = fmt.ok_or(WaveInspectionError::MalformedContent)?;
    let data_len = data_len.ok_or(WaveInspectionError::MalformedContent)?;
    let tag = u16::from_le_bytes(fmt[0..2].try_into().expect("format chunk"));
    if tag != 1 {
        return Err(WaveInspectionError::UnsupportedFormat);
    }
    let channels = u16::from_le_bytes(fmt[2..4].try_into().expect("format chunk"));
    let sample_rate = u32::from_le_bytes(fmt[4..8].try_into().expect("format chunk"));
    let byte_rate = u32::from_le_bytes(fmt[8..12].try_into().expect("format chunk"));
    let block_align = u16::from_le_bytes(fmt[12..14].try_into().expect("format chunk"));
    let bits_per_sample = u16::from_le_bytes(fmt[14..16].try_into().expect("format chunk"));
    if channels == 0
        || channels > MAX_WAVE_CHANNELS
        || sample_rate == 0
        || sample_rate > MAX_WAVE_SAMPLE_RATE
    {
        return Err(WaveInspectionError::ResourceLimit);
    }
    if !matches!(bits_per_sample, 8 | 16 | 24 | 32) {
        return Err(WaveInspectionError::UnsupportedFormat);
    }
    let expected_align = channels
        .checked_mul(bits_per_sample / 8)
        .ok_or(WaveInspectionError::MalformedContent)?;
    let expected_rate = sample_rate
        .checked_mul(u32::from(expected_align))
        .ok_or(WaveInspectionError::MalformedContent)?;
    if block_align != expected_align
        || byte_rate != expected_rate
        || data_len % usize::from(block_align) != 0
    {
        return Err(WaveInspectionError::MalformedContent);
    }
    let frame_count = u64::try_from(data_len / usize::from(block_align))
        .map_err(|_| WaveInspectionError::ResourceLimit)?;
    let duration_nanos = frame_count
        .checked_mul(1_000_000_000)
        .ok_or(WaveInspectionError::ResourceLimit)?
        / u64::from(sample_rate);
    Ok(PcmWaveProperties {
        channels,
        sample_rate,
        bits_per_sample,
        frame_count,
        duration_nanos,
        embedded_title: None,
    })
}
