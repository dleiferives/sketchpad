use serde::Serialize;
use sketchpad::{
    image_io::{decode_png_centered, encode_png, ExportRegion},
    raster::{LinearRgba, RasterLayer, TileCoord, DEFAULT_TILE_SIZE},
};
use std::{
    env,
    error::Error,
    hint::black_box,
    io::{Cursor, Write},
    process,
    time::{Duration, Instant},
};

const DEFAULT_SIZE: u32 = 2_048;
const DEFAULT_WARM_RUNS: usize = 7;

#[derive(Clone, Copy, Debug)]
enum Case {
    Transparent,
    SparseCenter,
    OpaqueGradient,
    TranslucentNoise,
}

impl Case {
    const ALL: [Self; 4] = [
        Self::Transparent,
        Self::SparseCenter,
        Self::OpaqueGradient,
        Self::TranslucentNoise,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::Transparent => "transparent",
            Self::SparseCenter => "sparse-center",
            Self::OpaqueGradient => "opaque-gradient",
            Self::TranslucentNoise => "translucent-noise",
        }
    }

    fn pixel(self, x: u32, y: u32, size: u32) -> [u8; 4] {
        match self {
            Self::Transparent => [0, 0, 0, 0],
            Self::SparseCenter => {
                let radius = (size / 16).max(1);
                let center = size / 2;
                if x.abs_diff(center) < radius && y.abs_diff(center) < radius {
                    [220, 70, 30, 192]
                } else {
                    [0, 0, 0, 0]
                }
            }
            Self::OpaqueGradient => [
                scale_to_byte(x, size),
                scale_to_byte(y, size),
                scale_to_byte(x ^ y, size.next_power_of_two()),
                255,
            ],
            Self::TranslucentNoise => {
                let hash = coordinate_hash(x, y);
                [
                    hash as u8,
                    (hash >> 8) as u8,
                    (hash >> 16) as u8,
                    32 + ((hash >> 24) as u8 % 224),
                ]
            }
        }
    }
}

#[derive(Debug)]
struct Configuration {
    size: u32,
    warm_runs: usize,
}

#[derive(Clone, Copy, Debug, Serialize)]
struct TimingSummary {
    first_micros: u64,
    warm_min_micros: u64,
    warm_median_micros: u64,
    warm_p95_micros: u64,
    warm_max_micros: u64,
}

#[derive(Debug, Serialize)]
struct Record {
    schema: &'static str,
    case: &'static str,
    width: u32,
    height: u32,
    warm_runs: usize,
    source_png_bytes: usize,
    decoded_frame_bytes: usize,
    placed_pixels: u64,
    allocated_tiles: usize,
    exported_png_bytes: usize,
    raster_checksum: String,
    exported_png_checksum: String,
    import: TimingSummary,
    export: TimingSummary,
}

fn main() {
    let configuration = parse_configuration().unwrap_or_else(|message| {
        eprintln!("{message}");
        process::exit(2);
    });
    for case in Case::ALL {
        let record = run_case(case, &configuration).unwrap_or_else(|error| {
            eprintln!("PNG benchmark {} failed: {error}", case.name());
            process::exit(1);
        });
        println!("{}", serde_json::to_string(&record).unwrap());
    }
}

fn parse_configuration() -> Result<Configuration, String> {
    parse_arguments(env::args().skip(1))
}

fn parse_arguments(arguments: impl IntoIterator<Item = String>) -> Result<Configuration, String> {
    let mut size = DEFAULT_SIZE;
    let mut warm_runs = DEFAULT_WARM_RUNS;
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--size" => {
                size = parse_value(arguments.next(), "--size")?;
                if size == 0 || size > 8_192 {
                    return Err("--size must be between 1 and 8192".to_owned());
                }
            }
            "--warm-runs" => {
                warm_runs = parse_value(arguments.next(), "--warm-runs")?;
                if warm_runs == 0 || warm_runs > 100 {
                    return Err("--warm-runs must be between 1 and 100".to_owned());
                }
            }
            "-h" | "--help" => {
                println!("usage: png_bench [--size PIXELS] [--warm-runs N]");
                process::exit(0);
            }
            _ => return Err(format!("unknown argument: {argument}")),
        }
    }
    Ok(Configuration { size, warm_runs })
}

fn parse_value<T: std::str::FromStr>(value: Option<String>, flag: &str) -> Result<T, String> {
    value
        .ok_or_else(|| format!("{flag} requires a value"))?
        .parse()
        .map_err(|_| format!("{flag} has an invalid value"))
}

fn run_case(case: Case, configuration: &Configuration) -> Result<Record, Box<dyn Error>> {
    let size = configuration.size;
    let source = generate_source_png(case, size)?;

    let first_import_started = Instant::now();
    let imported = decode_png_centered(Cursor::new(&source), size, size, DEFAULT_TILE_SIZE)?;
    let first_import = first_import_started.elapsed();
    let expected_checksum = raster_checksum(&imported.raster);
    let import_summary = imported.summary;

    let mut warm_imports = Vec::with_capacity(configuration.warm_runs);
    for _ in 0..configuration.warm_runs {
        let started = Instant::now();
        let warm = decode_png_centered(Cursor::new(&source), size, size, DEFAULT_TILE_SIZE)?;
        warm_imports.push(started.elapsed());
        let checksum = raster_checksum(&warm.raster);
        if checksum != expected_checksum {
            return Err(format!(
                "warm import checksum {checksum:016x} differs from {expected_checksum:016x}"
            )
            .into());
        }
        black_box(warm.raster.allocated_tile_count());
    }

    let first_export_started = Instant::now();
    let mut exported = Vec::new();
    encode_png(&mut exported, &imported.raster, ExportRegion::FullCanvas)?;
    let first_export = first_export_started.elapsed();
    let expected_png_checksum = checksum_bytes(&exported);

    let mut warm_exports = Vec::with_capacity(configuration.warm_runs);
    for _ in 0..configuration.warm_runs {
        let mut output = Vec::new();
        let started = Instant::now();
        encode_png(&mut output, &imported.raster, ExportRegion::FullCanvas)?;
        warm_exports.push(started.elapsed());
        let checksum = checksum_bytes(&output);
        if checksum != expected_png_checksum {
            return Err(format!(
                "warm export checksum {checksum:016x} differs from {expected_png_checksum:016x}"
            )
            .into());
        }
        black_box(output.len());
    }

    let round_trip = decode_png_centered(Cursor::new(&exported), size, size, DEFAULT_TILE_SIZE)?;
    let round_trip_checksum = raster_checksum(&round_trip.raster);
    if round_trip_checksum != expected_checksum {
        return Err(format!(
            "decode-export-decode checksum {round_trip_checksum:016x} differs from \
             {expected_checksum:016x}"
        )
        .into());
    }

    Ok(Record {
        schema: "sketchpad-png-bench-v1",
        case: case.name(),
        width: size,
        height: size,
        warm_runs: configuration.warm_runs,
        source_png_bytes: source.len(),
        decoded_frame_bytes: import_summary.decoded_bytes,
        placed_pixels: import_summary.placed_pixels,
        allocated_tiles: import_summary.allocated_tiles,
        exported_png_bytes: exported.len(),
        raster_checksum: format!("{expected_checksum:016x}"),
        exported_png_checksum: format!("{expected_png_checksum:016x}"),
        import: summarize(first_import, warm_imports),
        export: summarize(first_export, warm_exports),
    })
}

fn generate_source_png(case: Case, size: u32) -> Result<Vec<u8>, png::EncodingError> {
    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, size, size);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
        let mut writer = encoder.write_header()?;
        let mut stream = writer.stream_writer_with_size(64 * 1024)?;
        let mut row = vec![0_u8; size as usize * 4];
        for y in 0..size {
            for (x, pixel) in row.chunks_exact_mut(4).enumerate() {
                pixel.copy_from_slice(&case.pixel(x as u32, y, size));
            }
            stream.write_all(&row)?;
        }
        stream.finish()?;
    }
    Ok(encoded)
}

fn summarize(first: Duration, mut warm: Vec<Duration>) -> TimingSummary {
    warm.sort_unstable();
    let p95 = (warm.len() * 95).div_ceil(100).saturating_sub(1);
    TimingSummary {
        first_micros: micros(first),
        warm_min_micros: micros(warm[0]),
        warm_median_micros: micros(warm[(warm.len() - 1) / 2]),
        warm_p95_micros: micros(warm[p95]),
        warm_max_micros: micros(*warm.last().unwrap()),
    }
}

fn micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

fn raster_checksum(raster: &RasterLayer) -> u64 {
    let mut checksum = Fnv64::new();
    checksum.write(&raster.width().to_le_bytes());
    checksum.write(&raster.height().to_le_bytes());
    checksum.write(&raster.tile_size().to_le_bytes());
    let mut coords: Vec<TileCoord> = raster.allocated_tile_coords().collect();
    coords.sort_unstable_by_key(|coord| (coord.y, coord.x));
    for coord in coords {
        checksum.write(&coord.x.to_le_bytes());
        checksum.write(&coord.y.to_le_bytes());
        let tile = raster
            .tile(coord)
            .expect("allocated tile coordinates must resolve");
        for pixel in tile.pixels() {
            checksum_pixel(&mut checksum, *pixel);
        }
    }
    checksum.finish()
}

fn checksum_pixel(checksum: &mut Fnv64, pixel: LinearRgba) {
    checksum.write(&pixel.r.to_bits().to_le_bytes());
    checksum.write(&pixel.g.to_bits().to_le_bytes());
    checksum.write(&pixel.b.to_bits().to_le_bytes());
    checksum.write(&pixel.a.to_bits().to_le_bytes());
}

fn checksum_bytes(bytes: &[u8]) -> u64 {
    let mut checksum = Fnv64::new();
    checksum.write(bytes);
    checksum.finish()
}

struct Fnv64(u64);

impl Fnv64 {
    const fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    const fn finish(self) -> u64 {
        self.0
    }
}

fn scale_to_byte(value: u32, extent: u32) -> u8 {
    if extent <= 1 {
        return 0;
    }
    (u64::from(value.min(extent - 1)) * 255 / u64::from(extent - 1)) as u8
}

fn coordinate_hash(x: u32, y: u32) -> u32 {
    let mut value = x
        .wrapping_mul(0x9e37_79b9)
        .wrapping_add(y.wrapping_mul(0x85eb_ca6b))
        .wrapping_add(0xc2b2_ae35);
    value ^= value >> 16;
    value = value.wrapping_mul(0x7feb_352d);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846c_a68b);
    value ^ (value >> 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_case_is_exact_through_the_codec() {
        let configuration = Configuration {
            size: 17,
            warm_runs: 1,
        };
        for case in Case::ALL {
            let record = run_case(case, &configuration).unwrap();
            assert_eq!(record.placed_pixels, 17 * 17);
        }
    }

    #[test]
    fn argument_limits_are_explicit() {
        assert_eq!(
            parse_arguments(["--size", "256", "--warm-runs", "3"].map(str::to_owned))
                .unwrap()
                .size,
            256
        );
        assert!(parse_arguments(["--warm-runs", "0"].map(str::to_owned)).is_err());
        assert!(parse_arguments(["--size", "8193"].map(str::to_owned)).is_err());
    }
}
