use orna_serving::media_preview::{
    MAX_WAVE_CHANNELS, MAX_WAVE_SAMPLE_RATE, WaveInspectionError, inspect_pcm_wave, is_wave_essence,
};

fn wave(channels: u16, rate: u32, bits: u16, samples: &[u8]) -> Vec<u8> {
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
    bytes
}

#[test]
fn inspects_pcm_bytes_and_reports_absent_optional_title() {
    let props = inspect_pcm_wave(wave(2, 48_000, 16, &[0; 16])).expect("valid PCM WAVE");
    assert_eq!(props.channels, 2);
    assert_eq!(props.sample_rate, 48_000);
    assert_eq!(props.bits_per_sample, 16);
    assert_eq!(props.frame_count, 4);
    assert_eq!(props.duration_nanos, 83_333);
    assert_eq!(props.embedded_title, None);
}

#[test]
fn wrong_magic_is_malformed_content_even_under_a_wave_declaration() {
    // ORNA-MEDIA-002: the audio/* declaration never substitutes for byte validation.
    for bytes in [
        vec![],
        b"RIFF\0\0\0\0NOPE".to_vec(),
        b"RIFF\x04\0\0\0WAVE".to_vec(),
        b"this is plainly not a RIFF/WAVE container".to_vec(),
    ] {
        assert_eq!(
            inspect_pcm_wave(bytes),
            Err(WaveInspectionError::MalformedContent),
            "a WAVE declaration with bad magic must be 422, not a format verdict"
        );
    }
}

#[test]
fn rejects_chunk_or_format_inconsistencies() {
    let mut malformed = wave(1, 8_000, 16, &[0, 0]);
    malformed[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(
        inspect_pcm_wave(malformed),
        Err(WaveInspectionError::MalformedContent)
    );
    assert_eq!(
        inspect_pcm_wave(wave(1, 8_000, 16, &[0])),
        Err(WaveInspectionError::MalformedContent)
    );
}

#[test]
fn format_resource_limits_have_a_distinct_typed_error() {
    assert_eq!(
        inspect_pcm_wave(wave(MAX_WAVE_CHANNELS + 1, 8_000, 16, &[])),
        Err(WaveInspectionError::ResourceLimit)
    );
    assert_eq!(
        inspect_pcm_wave(wave(1, MAX_WAVE_SAMPLE_RATE + 1, 16, &[])),
        Err(WaveInspectionError::ResourceLimit)
    );
}

#[test]
fn non_pcm_wave_is_classified_as_unsupported() {
    let mut bytes = wave(1, 8_000, 16, &[]);
    bytes[20..22].copy_from_slice(&3u16.to_le_bytes());
    assert_eq!(
        inspect_pcm_wave(bytes),
        Err(WaveInspectionError::UnsupportedFormat)
    );
}

#[test]
fn wave_essences_are_the_declared_wave_names() {
    for essence in ["audio/wav", "audio/x-wav", "audio/wave", "audio/vnd.wave"] {
        assert!(is_wave_essence(essence), "{essence}");
    }
    for essence in ["audio/mpeg", "application/octet-stream", "image/png"] {
        assert!(!is_wave_essence(essence), "{essence}");
    }
}
