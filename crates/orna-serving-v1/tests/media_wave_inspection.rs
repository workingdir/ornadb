use orna_serving_v1::media_preview::{
    MAX_WAVE_CHANNELS, MAX_WAVE_INPUT_BYTES, MAX_WAVE_SAMPLE_RATE, WaveInspectionError, inspect_pcm_wave,
};
use orna_value_v1::Blob;

fn wave(channels: u16, rate: u32, bits: u16, samples: &[u8], mime: &str) -> Blob {
    let align = channels * (bits / 8);
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * u32::from(align)).to_le_bytes());
    bytes.extend_from_slice(&align.to_le_bytes());
    bytes.extend_from_slice(&bits.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(samples.len() as u32).to_le_bytes());
    bytes.extend_from_slice(samples);
    let riff_len = (bytes.len() - 8) as u32;
    bytes[4..8].copy_from_slice(&riff_len.to_le_bytes());
    Blob::from_bytes_with_annotation(bytes, mime, None).expect("Blob annotation is valid")
}

#[test]
fn inspects_pcm_from_blob_bytes_and_reports_absent_optional_title() {
    let blob = wave(2, 48_000, 16, &[0; 16], "application/octet-stream");
    let props = inspect_pcm_wave(&blob).expect("valid PCM WAVE");
    assert_eq!(props.channels, 2);
    assert_eq!(props.sample_rate, 48_000);
    assert_eq!(props.bits_per_sample, 16);
    assert_eq!(props.frame_count, 4);
    assert_eq!(props.duration_nanos, 83_333);
    assert_eq!(props.embedded_title, None);
}

#[test]
fn rejects_malformed_riff_headers_and_chunk_or_format_inconsistencies() {
    for bytes in [vec![], b"RIFF\0\0\0\0NOPE".to_vec(), b"RIFF\x04\0\0\0WAVE".to_vec()] {
        let blob = Blob::from_bytes_with_annotation(bytes, "audio/wav", None).unwrap();
        assert!(matches!(inspect_pcm_wave(&blob), Err(WaveInspectionError::UnsupportedFormat | WaveInspectionError::MalformedContent)));
    }
    let mut malformed = wave(1, 8_000, 16, &[0, 0], "audio/wav").read(0, 64).unwrap();
    malformed[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    let blob = Blob::from_bytes_with_annotation(malformed, "audio/wav", None).unwrap();
    assert_eq!(inspect_pcm_wave(&blob), Err(WaveInspectionError::MalformedContent));
    let odd_samples = wave(1, 8_000, 16, &[0], "audio/wav");
    assert_eq!(inspect_pcm_wave(&odd_samples), Err(WaveInspectionError::MalformedContent));
}

#[test]
fn resource_limits_have_a_distinct_typed_error() {
    let too_many_channels = wave(MAX_WAVE_CHANNELS + 1, 8_000, 16, &[], "audio/wav");
    assert_eq!(inspect_pcm_wave(&too_many_channels), Err(WaveInspectionError::ResourceLimit));
    let too_fast = wave(1, MAX_WAVE_SAMPLE_RATE + 1, 16, &[], "audio/wav");
    assert_eq!(inspect_pcm_wave(&too_fast), Err(WaveInspectionError::ResourceLimit));
    let oversized = Blob::from_bytes_with_annotation(vec![0; (MAX_WAVE_INPUT_BYTES + 1) as usize], "audio/wav", None).unwrap();
    assert_eq!(inspect_pcm_wave(&oversized), Err(WaveInspectionError::ResourceLimit));
}

#[test]
fn non_pcm_wave_is_classified_as_unsupported() {
    let blob = wave(1, 8_000, 16, &[], "audio/wav");
    let mut bytes = blob.read(0, blob.length()).unwrap();
    bytes[20..22].copy_from_slice(&3u16.to_le_bytes());
    let blob = Blob::from_bytes_with_annotation(bytes, "audio/wav", None).unwrap();
    assert_eq!(inspect_pcm_wave(&blob), Err(WaveInspectionError::UnsupportedFormat));
}
