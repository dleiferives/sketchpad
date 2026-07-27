use crate::raster::{LinearRgba, RasterError, RasterLayer, RectU32, TileCoord};
use std::{
    error::Error,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Seek, Write},
    path::{Path, PathBuf},
};

const MAX_IMPORT_DIMENSION: u32 = 8_192;
const MAX_IMPORT_PIXELS: u64 = 32 * 1024 * 1024;
const MAX_DECODED_FRAME_BYTES: usize = 128 * 1024 * 1024;
const MAX_DECODER_WORKING_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportRegion {
    FullCanvas,
    ContentBounds,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportSummary {
    pub source_width: u32,
    pub source_height: u32,
    pub decoded_bytes: usize,
    pub placed_pixels: u64,
    pub allocated_tiles: usize,
    pub assumed_srgb: bool,
}

pub struct ImportedRaster {
    pub raster: RasterLayer,
    pub summary: ImportSummary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportSummary {
    pub width: u32,
    pub height: u32,
    pub pixels: u64,
    pub encoded_bytes: u64,
}

#[derive(Debug)]
pub enum ImageIoError {
    Io(io::Error),
    Decode(png::DecodingError),
    Encode(png::EncodingError),
    Raster(RasterError),
    Invalid(String),
    Unsupported(String),
}

impl ImageIoError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }

    fn unsupported(message: impl Into<String>) -> Self {
        Self::Unsupported(message.into())
    }
}

impl fmt::Display for ImageIoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Decode(error) => error.fmt(formatter),
            Self::Encode(error) => error.fmt(formatter),
            Self::Raster(error) => error.fmt(formatter),
            Self::Invalid(message) => write!(formatter, "invalid PNG: {message}"),
            Self::Unsupported(message) => write!(formatter, "unsupported PNG: {message}"),
        }
    }
}

impl Error for ImageIoError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Decode(error) => Some(error),
            Self::Encode(error) => Some(error),
            Self::Raster(error) => Some(error),
            Self::Invalid(_) | Self::Unsupported(_) => None,
        }
    }
}

impl From<io::Error> for ImageIoError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<png::DecodingError> for ImageIoError {
    fn from(value: png::DecodingError) -> Self {
        Self::Decode(value)
    }
}

impl From<png::EncodingError> for ImageIoError {
    fn from(value: png::EncodingError) -> Self {
        Self::Encode(value)
    }
}

impl From<RasterError> for ImageIoError {
    fn from(value: RasterError) -> Self {
        Self::Raster(value)
    }
}

pub fn import_png_file(
    path: &Path,
    canvas_width: u32,
    canvas_height: u32,
    tile_size: u32,
) -> Result<ImportedRaster, ImageIoError> {
    let file = File::open(path)?;
    decode_png_centered(BufReader::new(file), canvas_width, canvas_height, tile_size)
}

pub fn decode_png_centered<R: Read + Seek>(
    source: R,
    canvas_width: u32,
    canvas_height: u32,
    tile_size: u32,
) -> Result<ImportedRaster, ImageIoError> {
    let limits = png::Limits {
        bytes: MAX_DECODER_WORKING_BYTES,
    };
    let mut decoder = png::Decoder::new_with_limits(BufReader::new(source), limits);
    decoder.set_ignore_text_chunk(true);
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info()?;

    let info = reader.info();
    validate_source_info(info)?;
    let source_width = info.width;
    let source_height = info.height;
    let assumed_srgb = info.srgb.is_none();

    let decoded_bytes = reader
        .output_buffer_size()
        .ok_or_else(|| ImageIoError::invalid("decoded frame size overflows this platform"))?;
    if decoded_bytes > MAX_DECODED_FRAME_BYTES {
        return Err(ImageIoError::invalid(format!(
            "decoded frame requires {decoded_bytes} bytes; limit is {MAX_DECODED_FRAME_BYTES}"
        )));
    }

    let mut decoded = vec![0_u8; decoded_bytes];
    let output = reader.next_frame(&mut decoded)?;
    let frame_bytes = output.buffer_size();
    decoded.truncate(frame_bytes);
    if output.width != source_width || output.height != source_height {
        return Err(ImageIoError::unsupported(
            "the decoded frame does not cover the PNG canvas",
        ));
    }

    let mut raster = RasterLayer::new(canvas_width, canvas_height, tile_size)?;
    // PNG RGBA8 has only 256 encoded color values. Computing the reference
    // transfer once preserves exact f32 results without three powf calls per
    // placed pixel. Sixteen-bit input still evaluates the continuous transfer.
    let srgb8_to_linear = std::array::from_fn(|sample| srgb_to_linear(sample as f32 / 255.0));
    let origin_x = (i64::from(canvas_width) - i64::from(source_width)).div_euclid(2);
    let origin_y = (i64::from(canvas_height) - i64::from(source_height)).div_euclid(2);
    let placed = intersect_placed_image(
        canvas_width,
        canvas_height,
        source_width,
        source_height,
        origin_x,
        origin_y,
    );

    let mut placed_pixels = 0;
    if let Some(destination) = placed {
        placed_pixels = destination.area();
        let min_tile_x = destination.min_x() / tile_size;
        let min_tile_y = destination.min_y() / tile_size;
        let max_tile_x = (destination.max_x() - 1) / tile_size;
        let max_tile_y = (destination.max_y() - 1) / tile_size;
        let tile_pixel_count = usize::try_from(
            tile_size
                .checked_mul(tile_size)
                .ok_or(RasterError::TileStorageTooLarge)?,
        )
        .map_err(|_| RasterError::TileStorageTooLarge)?;

        for tile_y in min_tile_y..=max_tile_y {
            for tile_x in min_tile_x..=max_tile_x {
                let coord = TileCoord::new(tile_x, tile_y);
                let tile_bounds = raster
                    .tile_bounds(coord)
                    .expect("placement tiles were clipped to the canvas");
                let overlap = intersection(tile_bounds, destination)
                    .expect("enumerated placement tiles overlap the image");
                let mut pixels = vec![LinearRgba::TRANSPARENT; tile_pixel_count].into_boxed_slice();
                for destination_y in overlap.min_y()..overlap.max_y() {
                    let source_y = (i64::from(destination_y) - origin_y) as u32;
                    let tile_row =
                        (destination_y - tile_bounds.min_y()) as usize * tile_size as usize;
                    for destination_x in overlap.min_x()..overlap.max_x() {
                        let source_x = (i64::from(destination_x) - origin_x) as u32;
                        let source_index =
                            u64::from(source_y) * u64::from(source_width) + u64::from(source_x);
                        let destination_index =
                            tile_row + (destination_x - tile_bounds.min_x()) as usize;
                        pixels[destination_index] = decode_pixel(
                            &decoded,
                            source_index as usize,
                            output.color_type,
                            output.bit_depth,
                            &srgb8_to_linear,
                        )?;
                    }
                }
                raster.restore_tile(coord, pixels)?;
            }
        }
    }

    Ok(ImportedRaster {
        summary: ImportSummary {
            source_width,
            source_height,
            decoded_bytes: frame_bytes,
            placed_pixels,
            allocated_tiles: raster.allocated_tile_count(),
            assumed_srgb,
        },
        raster,
    })
}

pub fn encode_png<W: Write>(
    destination: &mut W,
    raster: &RasterLayer,
    region: ExportRegion,
) -> Result<ExportSummary, ImageIoError> {
    let bounds = export_bounds(raster, region)?;
    let mut encoder = png::Encoder::new(destination, bounds.width(), bounds.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    let mut writer = encoder.write_header()?;
    let mut stream = writer.stream_writer_with_size(64 * 1024)?;
    let mut row = vec![0_u8; bounds.width() as usize * 4];

    for y in bounds.min_y()..bounds.max_y() {
        encode_row(raster, bounds.min_x(), bounds.max_x(), y, &mut row);
        stream.write_all(&row)?;
    }
    stream.finish()?;

    Ok(ExportSummary {
        width: bounds.width(),
        height: bounds.height(),
        pixels: bounds.area(),
        encoded_bytes: 0,
    })
}

pub fn export_png_file_atomic(
    path: &Path,
    raster: &RasterLayer,
    region: ExportRegion,
) -> Result<ExportSummary, ImageIoError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        fs::create_dir_all(parent)?;
    }
    let parent = parent.unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ImageIoError::invalid("export path has no UTF-8 file name"))?;
    let (temporary_path, mut file) = reserve_temporary(parent, file_name)?;

    let result = (|| {
        let mut buffered = BufWriter::new(&mut file);
        let mut summary = encode_png(&mut buffered, raster, region)?;
        buffered.flush()?;
        drop(buffered);
        file.sync_all()?;
        summary.encoded_bytes = file.metadata()?.len();
        fs::rename(&temporary_path, path)?;
        sync_directory(parent)?;
        Ok(summary)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

fn validate_source_info(info: &png::Info<'_>) -> Result<(), ImageIoError> {
    if info.width == 0 || info.height == 0 {
        return Err(ImageIoError::invalid("image dimensions must be nonzero"));
    }
    if info.width > MAX_IMPORT_DIMENSION || info.height > MAX_IMPORT_DIMENSION {
        return Err(ImageIoError::invalid(format!(
            "image dimensions {}x{} exceed the {}-pixel side limit",
            info.width, info.height, MAX_IMPORT_DIMENSION
        )));
    }
    let pixels = u64::from(info.width) * u64::from(info.height);
    if pixels > MAX_IMPORT_PIXELS {
        return Err(ImageIoError::invalid(format!(
            "image has {pixels} pixels; limit is {MAX_IMPORT_PIXELS}"
        )));
    }
    if info.animation_control.is_some() {
        return Err(ImageIoError::unsupported("animated PNG"));
    }
    if info.icc_profile.is_some() {
        return Err(ImageIoError::unsupported("embedded ICC color profile"));
    }
    if info.coding_independent_code_points.is_some()
        || info.mastering_display_color_volume.is_some()
        || info.content_light_level.is_some()
    {
        return Err(ImageIoError::unsupported("CICP or HDR color metadata"));
    }
    if info.srgb.is_none() && (info.gama_chunk.is_some() || info.chrm_chunk.is_some()) {
        return Err(ImageIoError::unsupported(
            "non-sRGB gamma or chromaticity metadata",
        ));
    }
    Ok(())
}

fn decode_pixel(
    bytes: &[u8],
    pixel_index: usize,
    color_type: png::ColorType,
    bit_depth: png::BitDepth,
    srgb8_to_linear: &[f32; 256],
) -> Result<LinearRgba, ImageIoError> {
    let channels = match color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::Rgb => 3,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => {
            return Err(ImageIoError::invalid(
                "palette expansion did not produce direct color samples",
            ))
        }
    };
    let sample_bytes = match bit_depth {
        png::BitDepth::Eight => 1,
        png::BitDepth::Sixteen => 2,
        other => {
            return Err(ImageIoError::invalid(format!(
                "sample depth {other:?} remained after expansion"
            )))
        }
    };
    let pixel_bytes = channels * sample_bytes;
    let offset = pixel_index
        .checked_mul(pixel_bytes)
        .ok_or_else(|| ImageIoError::invalid("decoded pixel offset overflow"))?;
    let samples = bytes
        .get(offset..offset + pixel_bytes)
        .ok_or_else(|| ImageIoError::invalid("decoded frame is shorter than declared"))?;
    let encoded_sample = |channel: usize| -> f32 {
        let offset = channel * sample_bytes;
        match sample_bytes {
            1 => f32::from(samples[offset]) / 255.0,
            2 => f32::from(u16::from_be_bytes([samples[offset], samples[offset + 1]])) / 65_535.0,
            _ => unreachable!(),
        }
    };
    let linear_sample = |channel: usize| -> f32 {
        let offset = channel * sample_bytes;
        match sample_bytes {
            1 => srgb8_to_linear[samples[offset] as usize],
            2 => srgb_to_linear(
                f32::from(u16::from_be_bytes([samples[offset], samples[offset + 1]])) / 65_535.0,
            ),
            _ => unreachable!(),
        }
    };
    let (red, green, blue, alpha) = match color_type {
        png::ColorType::Grayscale => {
            let gray = linear_sample(0);
            (gray, gray, gray, 1.0)
        }
        png::ColorType::Rgb => (linear_sample(0), linear_sample(1), linear_sample(2), 1.0),
        png::ColorType::GrayscaleAlpha => {
            let gray = linear_sample(0);
            (gray, gray, gray, encoded_sample(1))
        }
        png::ColorType::Rgba => (
            linear_sample(0),
            linear_sample(1),
            linear_sample(2),
            encoded_sample(3),
        ),
        png::ColorType::Indexed => unreachable!(),
    };
    Ok(LinearRgba::from_straight(red, green, blue, alpha))
}

fn encode_row(raster: &RasterLayer, min_x: u32, max_x: u32, y: u32, output: &mut [u8]) {
    let tile_size = raster.tile_size();
    let tile_y = y / tile_size;
    let first_tile_x = min_x / tile_size;
    let last_tile_x = (max_x - 1) / tile_size;

    for tile_x in first_tile_x..=last_tile_x {
        let coord = TileCoord::new(tile_x, tile_y);
        let tile_bounds = raster
            .tile_bounds(coord)
            .expect("export bounds lie within the raster");
        let span_min = min_x.max(tile_bounds.min_x());
        let span_max = max_x.min(tile_bounds.max_x());
        let destination_offset = (span_min - min_x) as usize * 4;
        let pixel_count = (span_max - span_min) as usize;

        if let Some(tile) = raster.tile(coord) {
            let source_row = (y - tile_bounds.min_y()) as usize * tile.stride();
            let source_offset = source_row + (span_min - tile_bounds.min_x()) as usize;
            for (pixel, encoded) in tile.pixels()[source_offset..source_offset + pixel_count]
                .iter()
                .copied()
                .zip(
                    output[destination_offset..destination_offset + pixel_count * 4]
                        .chunks_exact_mut(4),
                )
            {
                encoded.copy_from_slice(&encode_pixel(pixel));
            }
        } else {
            output[destination_offset..destination_offset + pixel_count * 4].fill(0);
        }
    }
}

fn encode_pixel(pixel: LinearRgba) -> [u8; 4] {
    let alpha = pixel.a.clamp(0.0, 1.0);
    if alpha == 0.0 {
        return [0, 0, 0, 0];
    }
    let inverse_alpha = alpha.recip();
    [
        quantize(linear_to_srgb((pixel.r * inverse_alpha).clamp(0.0, 1.0))),
        quantize(linear_to_srgb((pixel.g * inverse_alpha).clamp(0.0, 1.0))),
        quantize(linear_to_srgb((pixel.b * inverse_alpha).clamp(0.0, 1.0))),
        quantize(alpha),
    ]
}

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(value: f32) -> f32 {
    if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

fn quantize(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn export_bounds(raster: &RasterLayer, region: ExportRegion) -> Result<RectU32, ImageIoError> {
    match region {
        ExportRegion::FullCanvas => RectU32::from_xywh(0, 0, raster.width(), raster.height())
            .ok_or_else(|| ImageIoError::invalid("raster has empty dimensions")),
        ExportRegion::ContentBounds => raster
            .content_bounds()
            .ok_or_else(|| ImageIoError::invalid("cannot export empty content bounds")),
    }
}

fn intersect_placed_image(
    canvas_width: u32,
    canvas_height: u32,
    source_width: u32,
    source_height: u32,
    origin_x: i64,
    origin_y: i64,
) -> Option<RectU32> {
    let min_x = origin_x.max(0).min(i64::from(canvas_width)) as u32;
    let min_y = origin_y.max(0).min(i64::from(canvas_height)) as u32;
    let max_x = (origin_x + i64::from(source_width))
        .max(0)
        .min(i64::from(canvas_width)) as u32;
    let max_y = (origin_y + i64::from(source_height))
        .max(0)
        .min(i64::from(canvas_height)) as u32;
    RectU32::from_min_max(min_x, min_y, max_x, max_y)
}

fn intersection(a: RectU32, b: RectU32) -> Option<RectU32> {
    RectU32::from_min_max(
        a.min_x().max(b.min_x()),
        a.min_y().max(b.min_y()),
        a.max_x().min(b.max_x()),
        a.max_y().min(b.max_y()),
    )
}

fn reserve_temporary(parent: &Path, file_name: &str) -> Result<(PathBuf, File), ImageIoError> {
    for attempt in 0..100_u32 {
        let path = parent.join(format!(".{file_name}.tmp-{}-{attempt}", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(ImageIoError::invalid(
        "could not reserve a temporary export file",
    ))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), ImageIoError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), ImageIoError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn make_png(
        width: u32,
        height: u32,
        color: png::ColorType,
        depth: png::BitDepth,
        pixels: &[u8],
        mark_srgb: bool,
    ) -> Vec<u8> {
        let mut encoded = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut encoded, width, height);
            encoder.set_color(color);
            encoder.set_depth(depth);
            if mark_srgb {
                encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
            }
            encoder
                .write_header()
                .unwrap()
                .write_image_data(pixels)
                .unwrap();
        }
        encoded
    }

    #[test]
    fn imports_untagged_straight_alpha_as_centered_premultiplied_linear() {
        let png = make_png(
            1,
            1,
            png::ColorType::Rgba,
            png::BitDepth::Eight,
            &[128, 64, 32, 128],
            false,
        );
        let imported = decode_png_centered(Cursor::new(png), 3, 3, 2).unwrap();
        let pixel = imported.raster.pixel(1, 1).unwrap();
        let alpha = 128.0 / 255.0;
        assert!((pixel.r - srgb_to_linear(128.0 / 255.0) * alpha).abs() < 1.0e-6);
        assert!((pixel.g - srgb_to_linear(64.0 / 255.0) * alpha).abs() < 1.0e-6);
        assert!((pixel.b - srgb_to_linear(32.0 / 255.0) * alpha).abs() < 1.0e-6);
        assert_eq!(pixel.a, alpha);
        assert_eq!(imported.summary.placed_pixels, 1);
        assert!(imported.summary.assumed_srgb);
    }

    #[test]
    fn eight_bit_transfer_table_is_bit_exact_to_the_reference_function() {
        let table: [f32; 256] = std::array::from_fn(|sample| srgb_to_linear(sample as f32 / 255.0));
        for sample in 0_u16..=255 {
            assert_eq!(
                table[sample as usize].to_bits(),
                srgb_to_linear(f32::from(sample) / 255.0).to_bits()
            );
        }
    }

    #[test]
    fn imports_grayscale_alpha() {
        let png = make_png(
            2,
            1,
            png::ColorType::GrayscaleAlpha,
            png::BitDepth::Eight,
            &[255, 255, 128, 0],
            true,
        );
        let imported = decode_png_centered(Cursor::new(png), 2, 1, 2).unwrap();
        assert_eq!(
            imported.raster.pixel(0, 0),
            Some(LinearRgba::premultiplied(1.0, 1.0, 1.0, 1.0))
        );
        assert_eq!(imported.raster.pixel(1, 0), Some(LinearRgba::TRANSPARENT));
        assert!(!imported.summary.assumed_srgb);
    }

    #[test]
    fn centered_import_clips_larger_images_without_resampling() {
        let mut pixels = Vec::new();
        for value in 0_u8..25 {
            pixels.extend_from_slice(&[value, 0, 0, 255]);
        }
        let png = make_png(
            5,
            5,
            png::ColorType::Rgba,
            png::BitDepth::Eight,
            &pixels,
            true,
        );
        let imported = decode_png_centered(Cursor::new(png), 3, 3, 2).unwrap();
        assert_eq!(imported.summary.placed_pixels, 9);
        let top_left = imported.raster.pixel(0, 0).unwrap();
        let bottom_right = imported.raster.pixel(2, 2).unwrap();
        assert!((top_left.r - srgb_to_linear(6.0 / 255.0)).abs() < 1.0e-6);
        assert!((bottom_right.r - srgb_to_linear(18.0 / 255.0)).abs() < 1.0e-6);
    }

    #[test]
    fn export_is_straight_alpha_srgb_and_carries_srgb_metadata() {
        let mut raster = RasterLayer::new(2, 1, 2).unwrap();
        let gesture = raster.begin_gesture().unwrap();
        raster
            .set_pixel(
                gesture,
                0,
                0,
                LinearRgba::from_straight(srgb_to_linear(0.5), 0.0, 1.0, 0.5),
            )
            .unwrap();
        raster.commit_gesture(gesture).unwrap();

        let mut encoded = Vec::new();
        let summary = encode_png(&mut encoded, &raster, ExportRegion::FullCanvas).unwrap();
        assert_eq!(summary.pixels, 2);

        let decoder = png::Decoder::new(Cursor::new(encoded));
        let mut reader = decoder.read_info().unwrap();
        assert!(reader.info().srgb.is_some());
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let output = reader.next_frame(&mut pixels).unwrap();
        assert_eq!(
            &pixels[..output.buffer_size()],
            &[128, 0, 255, 128, 0, 0, 0, 0]
        );
    }

    #[test]
    fn content_bounds_export_has_exact_extent() {
        let mut raster = RasterLayer::new(10, 10, 4).unwrap();
        let gesture = raster.begin_gesture().unwrap();
        raster
            .set_pixel(
                gesture,
                7,
                8,
                LinearRgba::premultiplied(0.25, 0.5, 0.75, 1.0),
            )
            .unwrap();
        raster.commit_gesture(gesture).unwrap();
        let mut encoded = Vec::new();
        let summary = encode_png(&mut encoded, &raster, ExportRegion::ContentBounds).unwrap();
        assert_eq!([summary.width, summary.height], [1, 1]);
    }

    #[test]
    fn empty_content_bounds_export_is_explicitly_rejected() {
        let raster = RasterLayer::new(10, 10, 4).unwrap();
        let error = encode_png(&mut Vec::new(), &raster, ExportRegion::ContentBounds).unwrap_err();
        assert!(error.to_string().contains("empty content bounds"));
    }

    #[test]
    fn truncated_input_is_rejected_without_allocating_a_raster() {
        let mut png = make_png(
            1,
            1,
            png::ColorType::Rgba,
            png::BitDepth::Eight,
            &[1, 2, 3, 4],
            true,
        );
        png.truncate(png.len() / 2);
        assert!(decode_png_centered(Cursor::new(png), 8, 8, 4).is_err());
    }

    #[test]
    fn dimensions_and_custom_transfer_metadata_are_rejected_before_frame_allocation() {
        let oversized = png::Info::with_size(MAX_IMPORT_DIMENSION + 1, 1);
        assert!(validate_source_info(&oversized)
            .unwrap_err()
            .to_string()
            .contains("side limit"));

        let mut custom_gamma = png::Info::with_size(1, 1);
        custom_gamma.gama_chunk = Some(png::ScaledFloat::new(1.0));
        assert!(validate_source_info(&custom_gamma)
            .unwrap_err()
            .to_string()
            .contains("non-sRGB"));
    }
}
