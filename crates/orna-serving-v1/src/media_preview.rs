//! Explicit, bounded media inspection for stored Blobs.
//! Reads occur only on explicit request through the selected Blob's pinned resolver.
use std::io::Cursor;
use image::{ImageFormat, ImageReader, Limits as ImageLimits};
use orna_value_v1::Blob;
pub const MAX_PREVIEW_INPUT_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_PREVIEW_DIMENSION: u32 = 4096;
pub const MAX_PREVIEW_PIXELS: u64 = 4 * 1024 * 1024;
pub const MAX_DECODER_ALLOCATION: u64 = 16 * 1024 * 1024;
pub const MAX_WAVE_INPUT_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_WAVE_CHANNELS: u16 = 32;
pub const MAX_WAVE_SAMPLE_RATE: u32 = 384_000;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreviewError { UnsupportedMediaType, InputTooLarge, ContentUnavailable, MalformedOrOverLimit }
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedImage { pub width: u32, pub height: u32, pub rgba: Vec<u8> }
pub fn decode_image_preview(blob: &Blob) -> Result<DecodedImage, PreviewError> {
    let format = match blob.media_type().split(';').next().unwrap_or("").trim() {
        "image/png" => ImageFormat::Png, "image/jpeg" => ImageFormat::Jpeg,
        "image/gif" => ImageFormat::Gif, "image/webp" => ImageFormat::WebP,
        _ => return Err(PreviewError::UnsupportedMediaType),
    };
    let length = blob.length();
    if length > MAX_PREVIEW_INPUT_BYTES { return Err(PreviewError::InputTooLarge); }
    let bytes = blob.read(0, length).map_err(|_| PreviewError::ContentUnavailable)?;
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = ImageLimits::default();
    limits.max_image_width = Some(MAX_PREVIEW_DIMENSION);
    limits.max_image_height = Some(MAX_PREVIEW_DIMENSION);
    limits.max_alloc = Some(MAX_DECODER_ALLOCATION);
    reader.limits(limits);
    let decoded = reader.decode().map_err(|_| PreviewError::MalformedOrOverLimit)?;
    let (width, height) = (decoded.width(), decoded.height());
    if width == 0 || height == 0 || width > MAX_PREVIEW_DIMENSION || height > MAX_PREVIEW_DIMENSION || u64::from(width) * u64::from(height) > MAX_PREVIEW_PIXELS { return Err(PreviewError::MalformedOrOverLimit); }
    Ok(DecodedImage { width, height, rgba: decoded.into_rgba8().into_raw() })
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaveInspectionError { UnsupportedFormat, MalformedContent, ResourceLimit, ContentUnavailable }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PcmWaveProperties { pub channels: u16, pub sample_rate: u32, pub bits_per_sample: u16, pub frame_count: u64, pub duration_nanos: u64, pub embedded_title: Option<&'static str> }
pub fn inspect_pcm_wave(blob: &Blob) -> Result<PcmWaveProperties, WaveInspectionError> {
    let length = blob.length();
    if length > MAX_WAVE_INPUT_BYTES { return Err(WaveInspectionError::ResourceLimit); }
    let bytes = blob.read(0, length).map_err(|_| WaveInspectionError::ContentUnavailable)?;
    parse_pcm_wave(&bytes)
}
fn parse_pcm_wave(bytes: &[u8]) -> Result<PcmWaveProperties, WaveInspectionError> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" { return Err(WaveInspectionError::UnsupportedFormat); }
    let riff_size = u32::from_le_bytes(bytes[4..8].try_into().expect("four-byte field")) as usize;
    let riff_end = riff_size.checked_add(8).ok_or(WaveInspectionError::MalformedContent)?;
    if riff_end < 12 || riff_end != bytes.len() { return Err(WaveInspectionError::MalformedContent); }
    let mut cursor = 12usize; let mut fmt = None; let mut data_len = None;
    while cursor < riff_end {
        let header_end = cursor.checked_add(8).ok_or(WaveInspectionError::MalformedContent)?;
        if header_end > riff_end { return Err(WaveInspectionError::MalformedContent); }
        let id = &bytes[cursor..cursor + 4];
        let chunk_len = u32::from_le_bytes(bytes[cursor + 4..header_end].try_into().expect("four-byte field")) as usize;
        let payload_end = header_end.checked_add(chunk_len).ok_or(WaveInspectionError::MalformedContent)?;
        let padded_end = payload_end.checked_add(chunk_len & 1).ok_or(WaveInspectionError::MalformedContent)?;
        if payload_end > riff_end || padded_end > riff_end { return Err(WaveInspectionError::MalformedContent); }
        if id == b"fmt " { if fmt.is_some() || chunk_len < 16 { return Err(WaveInspectionError::MalformedContent); } fmt = Some(&bytes[header_end..payload_end]); }
        else if id == b"data" { if data_len.replace(chunk_len).is_some() { return Err(WaveInspectionError::MalformedContent); } }
        cursor = padded_end;
    }
    let fmt = fmt.ok_or(WaveInspectionError::MalformedContent)?; let data_len = data_len.ok_or(WaveInspectionError::MalformedContent)?;
    let tag = u16::from_le_bytes(fmt[0..2].try_into().expect("format chunk")); if tag != 1 { return Err(WaveInspectionError::UnsupportedFormat); }
    let channels = u16::from_le_bytes(fmt[2..4].try_into().expect("format chunk"));
    let sample_rate = u32::from_le_bytes(fmt[4..8].try_into().expect("format chunk"));
    let byte_rate = u32::from_le_bytes(fmt[8..12].try_into().expect("format chunk"));
    let block_align = u16::from_le_bytes(fmt[12..14].try_into().expect("format chunk"));
    let bits_per_sample = u16::from_le_bytes(fmt[14..16].try_into().expect("format chunk"));
    if channels == 0 || channels > MAX_WAVE_CHANNELS || sample_rate == 0 || sample_rate > MAX_WAVE_SAMPLE_RATE { return Err(WaveInspectionError::ResourceLimit); }
    if !matches!(bits_per_sample, 8 | 16 | 24 | 32) { return Err(WaveInspectionError::UnsupportedFormat); }
    let expected_align = channels.checked_mul(bits_per_sample / 8).ok_or(WaveInspectionError::MalformedContent)?;
    let expected_rate = sample_rate.checked_mul(u32::from(expected_align)).ok_or(WaveInspectionError::MalformedContent)?;
    if block_align != expected_align || byte_rate != expected_rate || data_len % usize::from(block_align) != 0 { return Err(WaveInspectionError::MalformedContent); }
    let frame_count = u64::try_from(data_len / usize::from(block_align)).map_err(|_| WaveInspectionError::ResourceLimit)?;
    let duration_nanos = frame_count.checked_mul(1_000_000_000).ok_or(WaveInspectionError::ResourceLimit)? / u64::from(sample_rate);
    Ok(PcmWaveProperties { channels, sample_rate, bits_per_sample, frame_count, duration_nanos, embedded_title: None })
}
