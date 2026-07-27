use crate::{
    document::{Document, DocumentError, DocumentLayerParts, LayerId},
    raster::{LinearRgba, RasterError, RasterLayer, TileCoord},
};
use std::{
    collections::HashSet,
    env,
    error::Error,
    ffi::OsString,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const MAGIC: [u8; 8] = *b"SKPRASTR";
const VERSION: u32 = 1;
const DOCUMENT_MAGIC: [u8; 8] = *b"SKPDOC02";
const DOCUMENT_VERSION: u32 = 1;
const FLAGS: u32 = 0;
const HEADER_SIZE: usize = 32;
const MAX_CANVAS_DIMENSION: u32 = 1_048_576;
const MAX_TILE_SIZE: u32 = 1_024;
const MAX_TILE_COUNT: u32 = 1_000_000;
const MAX_LAYER_COUNT: u32 = 1_024;
const MAX_LAYER_NAME_BYTES: usize = 4_096;
const MAX_CHECKPOINT_BYTES: u64 = 8 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CheckpointSummary {
    pub encoded_bytes: u64,
    pub layer_count: u32,
    pub tile_count: u32,
    pub stored_pixels: u64,
}

#[derive(Debug)]
pub enum CheckpointError {
    Io(io::Error),
    Invalid(String),
    Raster(RasterError),
    Document(DocumentError),
}

impl CheckpointError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

impl fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::Invalid(message) => write!(f, "invalid Sketchpad checkpoint: {message}"),
            Self::Raster(error) => error.fmt(f),
            Self::Document(error) => error.fmt(f),
        }
    }
}

impl Error for CheckpointError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Raster(error) => Some(error),
            Self::Document(error) => Some(error),
            Self::Invalid(_) => None,
        }
    }
}

impl From<io::Error> for CheckpointError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<RasterError> for CheckpointError {
    fn from(value: RasterError) -> Self {
        Self::Raster(value)
    }
}

impl From<DocumentError> for CheckpointError {
    fn from(value: DocumentError) -> Self {
        Self::Document(value)
    }
}

pub fn default_recovery_path() -> PathBuf {
    if let Some(path) = nonempty_env("SKETCHPAD_CHECKPOINT_PATH") {
        return PathBuf::from(path);
    }
    if let Some(state_home) = nonempty_env("XDG_STATE_HOME") {
        return PathBuf::from(state_home)
            .join("sketchpad")
            .join("recovery.skpr");
    }
    if let Some(user_home) = nonempty_env("HOME") {
        return PathBuf::from(user_home)
            .join(".local")
            .join("state")
            .join("sketchpad")
            .join("recovery.skpr");
    }
    env::temp_dir().join("sketchpad-recovery.skpr")
}

pub fn encode(layer: &RasterLayer) -> Result<(Vec<u8>, CheckpointSummary), CheckpointError> {
    validate_geometry(layer.width(), layer.height(), layer.tile_size())?;
    let tile_count = u32::try_from(layer.allocated_tile_count())
        .map_err(|_| CheckpointError::invalid("allocated tile count does not fit u32"))?;
    if tile_count > MAX_TILE_COUNT {
        return Err(CheckpointError::invalid(
            "allocated tile count exceeds limit",
        ));
    }

    let mut payload = Vec::new();
    push_u32(&mut payload, layer.width());
    push_u32(&mut payload, layer.height());
    push_u32(&mut payload, layer.tile_size());
    push_u32(&mut payload, tile_count);

    let mut coords: Vec<_> = layer.allocated_tile_coords().collect();
    coords.sort_unstable_by_key(|coord| (coord.y, coord.x));
    let mut stored_pixels = 0_u64;

    for coord in coords {
        let tile = layer
            .tile(coord)
            .expect("coordinates came from allocated tiles");
        push_u32(&mut payload, coord.x);
        push_u32(&mut payload, coord.y);
        let run_count_offset = payload.len();
        push_u32(&mut payload, 0);
        let mut run_count = 0_u32;

        for y in 0..tile.bounds().height() {
            let row_start = y as usize * tile.stride();
            let mut x = 0;
            while x < tile.bounds().width() {
                while x < tile.bounds().width()
                    && tile.pixels()[row_start + x as usize] == LinearRgba::TRANSPARENT
                {
                    x += 1;
                }
                if x == tile.bounds().width() {
                    break;
                }
                let run_start = x;
                while x < tile.bounds().width()
                    && tile.pixels()[row_start + x as usize] != LinearRgba::TRANSPARENT
                {
                    x += 1;
                }
                let run_length = x - run_start;
                push_u32(&mut payload, y);
                push_u32(&mut payload, run_start);
                push_u32(&mut payload, run_length);
                for pixel in &tile.pixels()[row_start + run_start as usize..row_start + x as usize]
                {
                    validate_pixel(*pixel)?;
                    push_pixel(&mut payload, *pixel);
                }
                run_count = run_count
                    .checked_add(1)
                    .ok_or_else(|| CheckpointError::invalid("tile run count overflow"))?;
                stored_pixels += u64::from(run_length);
            }
        }
        if run_count == 0 {
            return Err(CheckpointError::invalid(
                "allocated canonical tile contains no pixels",
            ));
        }
        payload[run_count_offset..run_count_offset + 4].copy_from_slice(&run_count.to_le_bytes());
    }

    let payload_len = u64::try_from(payload.len())
        .map_err(|_| CheckpointError::invalid("payload length does not fit u64"))?;
    let encoded_len = payload_len
        .checked_add(HEADER_SIZE as u64)
        .ok_or_else(|| CheckpointError::invalid("checkpoint length overflow"))?;
    if encoded_len > MAX_CHECKPOINT_BYTES {
        return Err(CheckpointError::invalid("checkpoint exceeds size limit"));
    }

    let mut encoded = Vec::with_capacity(HEADER_SIZE + payload.len());
    encoded.extend_from_slice(&MAGIC);
    push_u32(&mut encoded, VERSION);
    push_u32(&mut encoded, FLAGS);
    push_u64(&mut encoded, payload_len);
    push_u64(&mut encoded, checksum(&payload));
    encoded.extend_from_slice(&payload);
    Ok((
        encoded,
        CheckpointSummary {
            encoded_bytes: encoded_len,
            layer_count: 1,
            tile_count,
            stored_pixels,
        },
    ))
}

pub fn encode_document(
    document: &Document,
) -> Result<(Vec<u8>, CheckpointSummary), CheckpointError> {
    validate_geometry(document.width(), document.height(), document.tile_size())?;
    let layer_count = u32::try_from(document.layers().len())
        .map_err(|_| CheckpointError::invalid("layer count does not fit u32"))?;
    if layer_count == 0 || layer_count > MAX_LAYER_COUNT {
        return Err(CheckpointError::invalid("layer count is outside limits"));
    }

    let mut payload = Vec::new();
    push_u32(&mut payload, document.width());
    push_u32(&mut payload, document.height());
    push_u32(&mut payload, document.tile_size());
    push_u32(&mut payload, layer_count);
    push_u64(&mut payload, document.active_layer_id().get());

    let mut tile_count = 0_u32;
    let mut stored_pixels = 0_u64;
    for layer in document.layers() {
        let name = layer.name().as_bytes();
        if name.is_empty() || name.len() > MAX_LAYER_NAME_BYTES {
            return Err(CheckpointError::invalid(
                "layer name length is outside limits",
            ));
        }
        let (encoded_raster, summary) = encode(layer.raster())?;
        tile_count = tile_count
            .checked_add(summary.tile_count)
            .ok_or_else(|| CheckpointError::invalid("document tile count overflow"))?;
        if tile_count > MAX_TILE_COUNT {
            return Err(CheckpointError::invalid(
                "document tile count exceeds limit",
            ));
        }
        stored_pixels = stored_pixels
            .checked_add(summary.stored_pixels)
            .ok_or_else(|| CheckpointError::invalid("stored pixel count overflow"))?;

        push_u64(&mut payload, layer.id().get());
        push_u32(&mut payload, u32::from(layer.visible()));
        push_u32(&mut payload, layer.opacity().to_bits());
        push_u32(
            &mut payload,
            u32::try_from(name.len())
                .map_err(|_| CheckpointError::invalid("layer name length does not fit u32"))?,
        );
        payload.extend_from_slice(name);
        push_u64(
            &mut payload,
            u64::try_from(encoded_raster.len())
                .map_err(|_| CheckpointError::invalid("layer payload length does not fit u64"))?,
        );
        payload.extend_from_slice(&encoded_raster);
    }

    let payload_len = u64::try_from(payload.len())
        .map_err(|_| CheckpointError::invalid("payload length does not fit u64"))?;
    let encoded_len = payload_len
        .checked_add(HEADER_SIZE as u64)
        .ok_or_else(|| CheckpointError::invalid("checkpoint length overflow"))?;
    if encoded_len > MAX_CHECKPOINT_BYTES {
        return Err(CheckpointError::invalid("checkpoint exceeds size limit"));
    }
    let mut encoded = Vec::with_capacity(HEADER_SIZE + payload.len());
    encoded.extend_from_slice(&DOCUMENT_MAGIC);
    push_u32(&mut encoded, DOCUMENT_VERSION);
    push_u32(&mut encoded, FLAGS);
    push_u64(&mut encoded, payload_len);
    push_u64(&mut encoded, checksum(&payload));
    encoded.extend_from_slice(&payload);
    Ok((
        encoded,
        CheckpointSummary {
            encoded_bytes: encoded_len,
            layer_count,
            tile_count,
            stored_pixels,
        },
    ))
}

pub fn decode_document(encoded: &[u8]) -> Result<Document, CheckpointError> {
    let encoded_len = u64::try_from(encoded.len())
        .map_err(|_| CheckpointError::invalid("file length does not fit u64"))?;
    if encoded_len > MAX_CHECKPOINT_BYTES {
        return Err(CheckpointError::invalid("checkpoint exceeds size limit"));
    }

    let mut header = Reader::new(encoded);
    if header.take(DOCUMENT_MAGIC.len())? != DOCUMENT_MAGIC {
        return Err(CheckpointError::invalid(
            "document checkpoint magic does not match",
        ));
    }
    if header.u32()? != DOCUMENT_VERSION {
        return Err(CheckpointError::invalid(
            "unsupported document checkpoint version",
        ));
    }
    if header.u32()? != FLAGS {
        return Err(CheckpointError::invalid(
            "unsupported document feature flags",
        ));
    }
    let payload_len = usize::try_from(header.u64()?)
        .map_err(|_| CheckpointError::invalid("payload length does not fit usize"))?;
    let expected_checksum = header.u64()?;
    if payload_len != encoded.len().saturating_sub(HEADER_SIZE) {
        return Err(CheckpointError::invalid(
            "payload length does not match file",
        ));
    }
    let payload = header.take(payload_len)?;
    header.finish()?;
    if checksum(payload) != expected_checksum {
        return Err(CheckpointError::invalid("payload checksum does not match"));
    }

    let mut reader = Reader::new(payload);
    let width = reader.u32()?;
    let height = reader.u32()?;
    let tile_size = reader.u32()?;
    validate_geometry(width, height, tile_size)?;
    let layer_count = reader.u32()?;
    if layer_count == 0 || layer_count > MAX_LAYER_COUNT {
        return Err(CheckpointError::invalid("layer count is outside limits"));
    }
    let active_layer = LayerId::from_raw(reader.u64()?);
    let mut parts = Vec::with_capacity(layer_count as usize);
    let mut total_tiles = 0_u32;

    for _ in 0..layer_count {
        let id = LayerId::from_raw(reader.u64()?);
        let layer_flags = reader.u32()?;
        if layer_flags & !1 != 0 {
            return Err(CheckpointError::invalid("unsupported layer flags"));
        }
        let visible = layer_flags & 1 != 0;
        let opacity = f32::from_bits(reader.u32()?);
        let name_len = reader.u32()? as usize;
        if name_len == 0 || name_len > MAX_LAYER_NAME_BYTES {
            return Err(CheckpointError::invalid(
                "layer name length is outside limits",
            ));
        }
        let name = std::str::from_utf8(reader.take(name_len)?)
            .map_err(|_| CheckpointError::invalid("layer name is not UTF-8"))?
            .to_owned();
        let raster_len = usize::try_from(reader.u64()?)
            .map_err(|_| CheckpointError::invalid("layer payload length does not fit usize"))?;
        let raster_bytes = reader.take(raster_len)?;
        let raster = decode(raster_bytes)?;
        total_tiles = total_tiles
            .checked_add(
                u32::try_from(raster.allocated_tile_count())
                    .map_err(|_| CheckpointError::invalid("layer tile count does not fit u32"))?,
            )
            .ok_or_else(|| CheckpointError::invalid("document tile count overflow"))?;
        if total_tiles > MAX_TILE_COUNT {
            return Err(CheckpointError::invalid(
                "document tile count exceeds limit",
            ));
        }
        parts.push(DocumentLayerParts {
            id,
            name,
            visible,
            opacity,
            raster,
        });
    }
    reader.finish()?;
    Ok(Document::from_layer_parts(
        width,
        height,
        tile_size,
        active_layer,
        parts,
    )?)
}

pub fn decode(encoded: &[u8]) -> Result<RasterLayer, CheckpointError> {
    let encoded_len = u64::try_from(encoded.len())
        .map_err(|_| CheckpointError::invalid("file length does not fit u64"))?;
    if encoded_len > MAX_CHECKPOINT_BYTES {
        return Err(CheckpointError::invalid("checkpoint exceeds size limit"));
    }

    let mut header = Reader::new(encoded);
    if header.take(MAGIC.len())? != MAGIC {
        return Err(CheckpointError::invalid("magic does not match"));
    }
    if header.u32()? != VERSION {
        return Err(CheckpointError::invalid("unsupported version"));
    }
    if header.u32()? != FLAGS {
        return Err(CheckpointError::invalid("unsupported feature flags"));
    }
    let payload_len = usize::try_from(header.u64()?)
        .map_err(|_| CheckpointError::invalid("payload length does not fit usize"))?;
    let expected_checksum = header.u64()?;
    if payload_len != encoded.len().saturating_sub(HEADER_SIZE) {
        return Err(CheckpointError::invalid(
            "payload length does not match file",
        ));
    }
    let payload = header.take(payload_len)?;
    header.finish()?;
    if checksum(payload) != expected_checksum {
        return Err(CheckpointError::invalid("payload checksum does not match"));
    }

    let mut reader = Reader::new(payload);
    let width = reader.u32()?;
    let height = reader.u32()?;
    let tile_size = reader.u32()?;
    validate_geometry(width, height, tile_size)?;
    let tile_count = reader.u32()?;
    let tiles_wide = (width - 1) / tile_size + 1;
    let tiles_high = (height - 1) / tile_size + 1;
    let possible_tiles = u64::from(tiles_wide) * u64::from(tiles_high);
    if tile_count > MAX_TILE_COUNT || u64::from(tile_count) > possible_tiles {
        return Err(CheckpointError::invalid("tile count exceeds canvas bounds"));
    }

    let tile_pixel_count = usize::try_from(
        tile_size
            .checked_mul(tile_size)
            .ok_or_else(|| CheckpointError::invalid("tile pixel count overflow"))?,
    )
    .map_err(|_| CheckpointError::invalid("tile pixel count does not fit usize"))?;
    let mut layer = RasterLayer::new(width, height, tile_size)?;
    let mut seen = HashSet::with_capacity(tile_count as usize);

    for _ in 0..tile_count {
        let coord = TileCoord::new(reader.u32()?, reader.u32()?);
        if !seen.insert(coord) {
            return Err(CheckpointError::invalid("duplicate tile coordinate"));
        }
        let bounds = layer
            .tile_bounds(coord)
            .ok_or_else(|| CheckpointError::invalid("tile coordinate lies outside canvas"))?;
        let run_count = reader.u32()?;
        if run_count == 0 || u64::from(run_count) > bounds.area() {
            return Err(CheckpointError::invalid("tile run count is invalid"));
        }

        let mut pixels = vec![LinearRgba::TRANSPARENT; tile_pixel_count].into_boxed_slice();
        let mut previous: Option<(u32, u32)> = None;
        for _ in 0..run_count {
            let y = reader.u32()?;
            let x = reader.u32()?;
            let length = reader.u32()?;
            let end = x
                .checked_add(length)
                .ok_or_else(|| CheckpointError::invalid("pixel run overflows"))?;
            if length == 0 || y >= bounds.height() || end > bounds.width() {
                return Err(CheckpointError::invalid("pixel run lies outside tile"));
            }
            if previous.is_some_and(|(previous_y, previous_end)| {
                y < previous_y || (y == previous_y && x < previous_end)
            }) {
                return Err(CheckpointError::invalid(
                    "pixel runs overlap or are not ordered",
                ));
            }
            previous = Some((y, end));

            let start = y as usize * tile_size as usize + x as usize;
            for index in 0..length as usize {
                let pixel = reader.pixel()?;
                validate_pixel(pixel)?;
                if pixel == LinearRgba::TRANSPARENT {
                    return Err(CheckpointError::invalid(
                        "transparent pixel appears inside stored run",
                    ));
                }
                pixels[start + index] = pixel;
            }
        }
        layer.restore_tile(coord, pixels)?;
    }
    reader.finish()?;
    Ok(layer)
}

pub fn save_atomic(path: &Path, layer: &RasterLayer) -> Result<CheckpointSummary, CheckpointError> {
    let (encoded, summary) = encode(layer)?;
    write_atomic(path, &encoded)?;
    Ok(summary)
}

pub fn save_document_atomic(
    path: &Path,
    document: &Document,
) -> Result<CheckpointSummary, CheckpointError> {
    let (encoded, summary) = encode_document(document)?;
    write_atomic(path, &encoded)?;
    Ok(summary)
}

fn write_atomic(path: &Path, encoded: &[u8]) -> Result<(), CheckpointError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .ok_or_else(|| CheckpointError::invalid("checkpoint path has no file name"))?
        .to_string_lossy();

    let mut temporary = None;
    for attempt in 0..100_u32 {
        let candidate = parent.join(format!(".{file_name}.tmp-{}-{attempt}", std::process::id()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    let (temporary_path, mut file) =
        temporary.ok_or_else(|| CheckpointError::invalid("could not reserve temporary file"))?;

    let write_result = (|| -> Result<(), CheckpointError> {
        file.write_all(&encoded)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary_path, path)?;
        sync_parent_directory(parent)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    write_result?;
    Ok(())
}

pub fn load(path: &Path) -> Result<RasterLayer, CheckpointError> {
    let encoded = read_checkpoint(path)?;
    decode(&encoded)
}

pub fn load_document(path: &Path) -> Result<Document, CheckpointError> {
    let encoded = read_checkpoint(path)?;
    if encoded.starts_with(&DOCUMENT_MAGIC) {
        decode_document(&encoded)
    } else if encoded.starts_with(&MAGIC) {
        Ok(Document::from_flattened(decode(&encoded)?, "Recovered")?)
    } else {
        Err(CheckpointError::invalid("checkpoint magic does not match"))
    }
}

fn read_checkpoint(path: &Path) -> Result<Vec<u8>, CheckpointError> {
    let file = File::open(path)?;
    let declared_length = file.metadata()?.len();
    if declared_length > MAX_CHECKPOINT_BYTES {
        return Err(CheckpointError::invalid("checkpoint exceeds size limit"));
    }
    let capacity = usize::try_from(declared_length)
        .map_err(|_| CheckpointError::invalid("file length does not fit usize"))?;
    let mut encoded = Vec::with_capacity(capacity);
    file.take(MAX_CHECKPOINT_BYTES + 1)
        .read_to_end(&mut encoded)?;
    if encoded.len() as u64 > MAX_CHECKPOINT_BYTES {
        return Err(CheckpointError::invalid("checkpoint exceeds size limit"));
    }
    Ok(encoded)
}

fn validate_geometry(width: u32, height: u32, tile_size: u32) -> Result<(), CheckpointError> {
    if width == 0 || height == 0 {
        return Err(CheckpointError::invalid(
            "canvas dimensions must be nonzero",
        ));
    }
    if width > MAX_CANVAS_DIMENSION || height > MAX_CANVAS_DIMENSION {
        return Err(CheckpointError::invalid("canvas dimensions exceed limit"));
    }
    if tile_size == 0 || tile_size > MAX_TILE_SIZE {
        return Err(CheckpointError::invalid(
            "tile size is outside supported range",
        ));
    }
    tile_size
        .checked_mul(tile_size)
        .ok_or_else(|| CheckpointError::invalid("tile pixel count overflow"))?;
    Ok(())
}

fn validate_pixel(pixel: LinearRgba) -> Result<(), CheckpointError> {
    let channels = [pixel.r, pixel.g, pixel.b, pixel.a];
    if channels.iter().any(|channel| !channel.is_finite()) {
        return Err(CheckpointError::invalid(
            "pixel contains a non-finite channel",
        ));
    }
    if !(0.0..=1.0).contains(&pixel.a)
        || pixel.r < 0.0
        || pixel.g < 0.0
        || pixel.b < 0.0
        || pixel.r > pixel.a
        || pixel.g > pixel.a
        || pixel.b > pixel.a
    {
        return Err(CheckpointError::invalid(
            "pixel violates premultiplied linear RGBA bounds",
        ));
    }
    Ok(())
}

fn nonempty_env(name: &str) -> Option<OsString> {
    env::var_os(name).filter(|value| !value.is_empty())
}

fn checksum(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_pixel(output: &mut Vec<u8>, pixel: LinearRgba) {
    for channel in [pixel.r, pixel.g, pixel.b, pixel.a] {
        push_u32(output, channel.to_bits());
    }
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> Result<(), io::Error> {
    File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_: &Path) -> Result<(), io::Error> {
    Ok(())
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], CheckpointError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or_else(|| CheckpointError::invalid("read offset overflow"))?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| CheckpointError::invalid("file is truncated"))?;
        self.position = end;
        Ok(value)
    }

    fn u32(&mut self) -> Result<u32, CheckpointError> {
        let bytes: [u8; 4] = self
            .take(4)?
            .try_into()
            .expect("the reader returned exactly four bytes");
        Ok(u32::from_le_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, CheckpointError> {
        let bytes: [u8; 8] = self
            .take(8)?
            .try_into()
            .expect("the reader returned exactly eight bytes");
        Ok(u64::from_le_bytes(bytes))
    }

    fn pixel(&mut self) -> Result<LinearRgba, CheckpointError> {
        Ok(LinearRgba {
            r: f32::from_bits(self.u32()?),
            g: f32::from_bits(self.u32()?),
            b: f32::from_bits(self.u32()?),
            a: f32::from_bits(self.u32()?),
        })
    }

    fn finish(self) -> Result<(), CheckpointError> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(CheckpointError::invalid("file contains trailing bytes"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_layer() -> RasterLayer {
        let mut layer = RasterLayer::new(300, 260, 128).unwrap();
        let mut gesture = layer.scoped_gesture().unwrap();
        for (x, y, color) in [
            (0, 0, LinearRgba::from_straight(0.8, 0.1, 0.2, 0.5)),
            (127, 20, LinearRgba::from_straight(0.1, 0.6, 0.2, 1.0)),
            (128, 20, LinearRgba::from_straight(0.2, 0.3, 0.9, 0.75)),
            (299, 259, LinearRgba::from_straight(0.5, 0.4, 0.1, 1.0)),
        ] {
            gesture.set_pixel(x, y, color).unwrap();
        }
        gesture.commit().unwrap();
        layer
    }

    fn sample_document() -> Document {
        let mut document = Document::from_flattened(sample_layer(), "Paint").unwrap();
        let paint = document.active_layer_id();
        let highlights = document.create_layer("Highlights").unwrap();
        {
            let mut gesture = document.active_layer_mut().scoped_gesture().unwrap();
            gesture
                .set_pixel(42, 43, LinearRgba::from_straight(0.9, 0.8, 0.2, 0.4))
                .unwrap();
            let damage = gesture.commit().unwrap().unwrap();
            document.recompose_damage(&damage).unwrap();
        }
        document.set_layer_opacity(highlights, 0.625).unwrap();
        let hidden = document.create_layer("Reference").unwrap();
        {
            let mut gesture = document.active_layer_mut().scoped_gesture().unwrap();
            gesture
                .set_pixel(7, 8, LinearRgba::from_straight(0.1, 0.9, 0.7, 1.0))
                .unwrap();
            let damage = gesture.commit().unwrap().unwrap();
            document.recompose_damage(&damage).unwrap();
        }
        document.set_layer_visibility(hidden, false).unwrap();
        document.set_active_layer(paint).unwrap();
        document
    }

    #[test]
    fn sparse_checkpoint_round_trip_is_exact_and_deterministic() {
        let layer = sample_layer();
        let (first, summary) = encode(&layer).unwrap();
        let (second, _) = encode(&layer).unwrap();
        assert_eq!(first, second);
        assert_eq!(summary.tile_count, 3);
        assert_eq!(summary.stored_pixels, 4);

        let restored = decode(&first).unwrap();
        assert_eq!(restored.width(), layer.width());
        assert_eq!(restored.height(), layer.height());
        assert_eq!(restored.tile_size(), layer.tile_size());
        assert_eq!(
            restored.allocated_tile_count(),
            layer.allocated_tile_count()
        );
        assert_eq!(restored.undo_depth(), 0);
        for y in 0..layer.height() {
            for x in 0..layer.width() {
                assert_eq!(restored.pixel(x, y), layer.pixel(x, y));
            }
        }
    }

    #[test]
    fn checksum_rejects_corrupted_payload() {
        let (mut encoded, _) = encode(&sample_layer()).unwrap();
        let last = encoded.last_mut().unwrap();
        *last ^= 0x80;
        assert!(matches!(
            decode(&encoded),
            Err(CheckpointError::Invalid(message)) if message.contains("checksum")
        ));
    }

    #[test]
    fn truncation_and_trailing_bytes_are_rejected() {
        let (encoded, _) = encode(&sample_layer()).unwrap();
        assert!(decode(&encoded[..encoded.len() - 1]).is_err());

        let mut trailing = encoded;
        trailing.push(0);
        assert!(decode(&trailing).is_err());
    }

    #[test]
    fn atomic_save_replaces_a_previous_checkpoint() {
        let unique = format!(
            "sketchpad-checkpoint-test-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("unnamed")
        );
        let directory = env::temp_dir().join(unique);
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("recovery.skpr");

        let layer = sample_layer();
        save_atomic(&path, &layer).unwrap();
        save_atomic(&path, &layer).unwrap();
        let restored = load(&path).unwrap();
        assert_eq!(restored.allocated_tile_count(), 3);

        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn layered_document_round_trip_is_exact_and_deterministic() {
        let document = sample_document();
        let (first, summary) = encode_document(&document).unwrap();
        let (second, _) = encode_document(&document).unwrap();
        assert_eq!(first, second);
        assert_eq!(summary.layer_count, 3);
        assert_eq!(summary.tile_count, 5);
        assert_eq!(summary.stored_pixels, 6);

        let restored = decode_document(&first).unwrap();
        assert_eq!(restored.width(), document.width());
        assert_eq!(restored.height(), document.height());
        assert_eq!(restored.tile_size(), document.tile_size());
        assert_eq!(
            restored.active_layer_id().get(),
            document.active_layer_id().get()
        );
        assert_eq!(restored.layers().len(), document.layers().len());
        for (expected, actual) in document.layers().iter().zip(restored.layers()) {
            assert_eq!(actual.id(), expected.id());
            assert_eq!(actual.name(), expected.name());
            assert_eq!(actual.visible(), expected.visible());
            assert_eq!(actual.opacity(), expected.opacity());
            assert_eq!(
                actual.raster().allocated_tile_count(),
                expected.raster().allocated_tile_count()
            );
            for coord in expected.raster().allocated_tile_coords() {
                assert_eq!(
                    actual.raster().tile(coord).unwrap().pixels(),
                    expected.raster().tile(coord).unwrap().pixels()
                );
            }
        }
        for coord in document.composite().allocated_tile_coords() {
            assert_eq!(
                restored.composite().tile(coord).unwrap().pixels(),
                document.composite().tile(coord).unwrap().pixels()
            );
        }
        assert_eq!(restored.composite_stats(), Default::default());
    }

    #[test]
    fn document_checksum_rejects_corruption() {
        let (mut encoded, _) = encode_document(&sample_document()).unwrap();
        *encoded.last_mut().unwrap() ^= 0x01;
        assert!(matches!(
            decode_document(&encoded),
            Err(CheckpointError::Invalid(message)) if message.contains("checksum")
        ));
    }

    #[test]
    fn document_loader_migrates_flat_checkpoint_without_changing_pixels() {
        let unique = format!(
            "sketchpad-document-migration-test-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("unnamed")
        );
        let directory = env::temp_dir().join(unique);
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("recovery.skpr");
        let layer = sample_layer();
        save_atomic(&path, &layer).unwrap();

        let restored = load_document(&path).unwrap();
        assert_eq!(restored.layers().len(), 1);
        assert_eq!(restored.layers()[0].name(), "Recovered");
        for coord in layer.allocated_tile_coords() {
            assert_eq!(
                restored.active_layer().tile(coord).unwrap().pixels(),
                layer.tile(coord).unwrap().pixels()
            );
        }

        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn atomic_document_save_replaces_previous_document() {
        let unique = format!(
            "sketchpad-document-checkpoint-test-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("unnamed")
        );
        let directory = env::temp_dir().join(unique);
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("recovery.skpr");
        let document = sample_document();
        save_document_atomic(&path, &document).unwrap();
        save_document_atomic(&path, &document).unwrap();

        let restored = load_document(&path).unwrap();
        assert_eq!(restored.layers().len(), 3);
        assert_eq!(restored.active_layer_id(), document.active_layer_id());

        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
