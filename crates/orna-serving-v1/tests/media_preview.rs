use std::io::Cursor;

use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use orna_serving_v1::media_preview::{
    DecodedImage, MAX_PREVIEW_DIMENSION, MAX_PREVIEW_INPUT_BYTES, PreviewError,
    decode_image_preview,
};
use orna_value_v1::Blob;

fn encoded_blob(format: ImageFormat, mime: &str, width: u32, height: u32) -> Blob {
    let image = RgbaImage::from_pixel(width, height, Rgba([10, 20, 30, 255]));
    let mut encoded = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut encoded, format)
        .expect("encode synthetic media fixture");
    Blob::from_bytes_with_annotation(encoded.into_inner(), mime, None)
        .expect("valid media annotation")
}

#[test]
fn explicitly_requested_raster_formats_decode_with_bounded_output() {
    for (format, mime) in [
        (ImageFormat::Png, "image/png"),
        (ImageFormat::Jpeg, "image/jpeg"),
        (ImageFormat::Gif, "image/gif"),
        (ImageFormat::WebP, "image/webp"),
    ] {
        let blob = encoded_blob(format, mime, 2, 1);
        let result = decode_image_preview(&blob).expect("supported pinned format decodes");
        assert_eq!((result.width, result.height), (2, 1));
        assert_eq!(result.rgba.len(), 8);
    }
}

#[test]
fn explicit_png_preview_returns_bounded_rgba_pixels() {
    let blob = encoded_blob(ImageFormat::Png, "image/png", 2, 1);
    let result = decode_image_preview(&blob).expect("valid synthetic PNG decodes");
    assert_eq!(
        result,
        DecodedImage {
            width: 2,
            height: 1,
            rgba: vec![10, 20, 30, 255, 10, 20, 30, 255],
        }
    );
    assert_eq!(blob.read(0, blob.length()).expect("raw bytes remain readable").len() as u64, blob.length());
}

/// ORNA-MEDIA-005: malformed and oversized inputs are rejected without panicking.
#[test]
fn malformed_inputs_and_oversized_dimensions_are_rejected_without_panicking() {
    let malformed = [
        vec![],
        b"not an image".to_vec(),
        b"\x89PNG\r\n\x1a\n".to_vec(),
        b"GIF89a\xff\xff\xff\xff".to_vec(),
        b"RIFF\xff\xff\xff\xffWEBPVP8 ".to_vec(),
    ];
    for bytes in malformed {
        let blob = Blob::from_bytes_with_annotation(bytes, "image/png", None)
            .expect("valid PNG declaration");
        assert_eq!(
            decode_image_preview(&blob),
            Err(PreviewError::MalformedOrOverLimit),
            "malformed fixture must fail closed"
        );
    }

    // Decoder dimension checks run before any pixel-buffer allocation.
    let too_wide = encoded_blob(
        ImageFormat::Png,
        "image/png",
        MAX_PREVIEW_DIMENSION + 1,
        1,
    );
    assert_eq!(
        decode_image_preview(&too_wide),
        Err(PreviewError::MalformedOrOverLimit)
    );

    // The dimensions are rejected from the header before pixel-buffer
    // allocation; malformed/missing CRC is irrelevant to the resource bound.
    let mut huge_png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    huge_png.extend_from_slice(&u32::MAX.to_be_bytes());
    huge_png.extend_from_slice(&u32::MAX.to_be_bytes());
    huge_png.extend_from_slice(&[8, 6, 0, 0, 0]);
    let blob = Blob::from_bytes_with_annotation(huge_png, "image/png", None)
        .expect("valid PNG declaration");
    assert_eq!(
        decode_image_preview(&blob),
        Err(PreviewError::MalformedOrOverLimit)
    );
}

#[test]
fn unsupported_active_content_is_not_read_or_decoded() {
    let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>throw 1</script></svg>";
    let blob = Blob::from_bytes_with_annotation(svg.to_vec(), "image/svg+xml", None)
        .expect("valid SVG declaration");
    let identity = blob.content_identity();
    assert_eq!(
        decode_image_preview(&blob),
        Err(PreviewError::UnsupportedMediaType)
    );
    assert_eq!(blob.content_identity(), identity);
    assert_eq!(blob.read(0, svg.len() as u64).expect("raw SVG is preservable"), svg);
}

/// ORNA-MEDIA-005: preview input over the limit is rejected with a typed error.
#[test]
fn preview_input_limit_rejects_before_attempting_full_blob_read() {
    let bytes = vec![0_u8; (MAX_PREVIEW_INPUT_BYTES + 1) as usize];
    let blob = Blob::from_bytes_with_annotation(bytes, "image/png", None)
        .expect("valid PNG declaration");
    assert_eq!(decode_image_preview(&blob), Err(PreviewError::InputTooLarge));
    assert_eq!(blob.length(), MAX_PREVIEW_INPUT_BYTES + 1);
}
