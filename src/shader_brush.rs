//! Versioned, coverage-union WGSL brush packages. File IO and reload compilation
//! run off the input thread; accepted pipelines are immutable for each contact.
use crate::{
    brush_tip::BrushTip,
    gpu_round_target::RoundMaskTarget,
    stroke::{RoundBrushRecipeV1, StrokeMaterial},
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

pub const BUILTIN_IDS: [&str; 5] = [
    "hard-round",
    "pencil",
    "marker",
    "palette-knife",
    "charcoal",
];
const MAX_SOURCE_BYTES: u64 = 256 * 1024;
const MAX_PACKAGES: usize = 128;

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrushManifest {
    pub api_version: u32,
    #[serde(default)]
    pub engine: Option<String>,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub footprint: Footprint,
    #[serde(default = "one")]
    pub extent: f32,
    #[serde(default = "default_diameter")]
    pub diameter: f32,
    #[serde(default = "one")]
    pub opacity: f32,
}
fn one() -> f32 {
    1.0
}
fn default_diameter() -> f32 {
    32.0
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Footprint {
    #[default]
    Constant,
    Round,
    Pencil,
    Marker,
    Knife,
    Charcoal,
}
impl Footprint {
    pub fn tip(self) -> BrushTip {
        match self {
            Self::Constant | Self::Round => BrushTip::HardRound,
            Self::Pencil => BrushTip::Pencil,
            Self::Marker => BrushTip::Marker,
            Self::Knife => BrushTip::PaletteKnife,
            Self::Charcoal => BrushTip::Charcoal,
        }
    }
}
impl BrushManifest {
    pub fn parse(source: &str) -> Result<Self, String> {
        let manifest: Self = serde_json::from_str(source).map_err(|e| e.to_string())?;
        if !((manifest.api_version == 1 && manifest.engine.is_none())
            || (manifest.api_version == 2 && manifest.engine.as_deref() == Some("loaded-paint")))
        {
            return Err("expected coverage API 1, or API 2 with engine loaded-paint".into());
        }
        if manifest.id.is_empty()
            || manifest.id.len() > 64
            || !manifest
                .id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        {
            return Err("brush id must use 1–64 lowercase letters, digits or hyphens".into());
        }
        if manifest.name.trim().is_empty()
            || manifest.name.len() > 80
            || manifest.description.len() > 256
        {
            return Err("brush name/description is empty or too long".into());
        }
        if !manifest.extent.is_finite()
            || !(0.125..=4.0).contains(&manifest.extent)
            || !manifest.diameter.is_finite()
            || !(1.0..=512.0).contains(&manifest.diameter)
            || !manifest.opacity.is_finite()
            || !(0.0..=1.0).contains(&manifest.opacity)
        {
            return Err("invalid brush extent, diameter or opacity".into());
        }
        Ok(manifest)
    }
    pub fn recipe(&self, material: StrokeMaterial, diameter: f32) -> RoundBrushRecipeV1 {
        RoundBrushRecipeV1::with_minimum_pressure_fraction(
            material,
            diameter * self.extent,
            if self.footprint == Footprint::Constant {
                1.0
            } else {
                0.05
            },
        )
        .expect("validated brush settings")
        .with_tip(self.footprint.tip())
    }
    pub fn radius(&self, diameter: f32, pressure: f32, tilt: [f32; 2]) -> f32 {
        self.footprint.tip().radius(
            diameter * self.extent,
            pressure,
            tilt,
            if self.footprint == Footprint::Constant {
                1.0
            } else {
                0.05
            },
        )
    }
}

#[derive(Clone)]
pub struct ShaderBrush {
    pub manifest: BrushManifest,
    pub pipeline: wgpu::RenderPipeline,
    source: Arc<str>,
}
type Rejections = BTreeMap<PathBuf, (String, String)>;
struct Reload {
    brushes: BTreeMap<String, ShaderBrush>,
    errors: Vec<String>,
    rejected: Rejections,
}

pub struct BrushLibrary {
    pub brushes: BTreeMap<String, ShaderBrush>,
    pub errors: Vec<String>,
    pub root: PathBuf,
    rejected: Rejections,
    next_scan: Instant,
    pending: Option<mpsc::Receiver<Reload>>,
}
impl BrushLibrary {
    pub fn new(
        device: &wgpu::Device,
        layout: &wgpu::PipelineLayout,
        root: PathBuf,
    ) -> Result<Self, String> {
        let mut brushes = BTreeMap::new();
        macro_rules! builtin {
            ($id:literal) => {{
                let manifest =
                    BrushManifest::parse(include_str!(concat!("../brushes/", $id, "/brush.json")))?;
                let source: Arc<str> =
                    include_str!(concat!("../brushes/", $id, "/brush.wgsl")).into();
                let pipeline = if manifest.api_version == 2 {
                    RoundMaskTarget::compile_material_brush(device, layout, &source)?
                } else {
                    RoundMaskTarget::compile_brush(device, layout, &source)?
                };
                brushes.insert(
                    manifest.id.clone(),
                    ShaderBrush {
                        manifest,
                        pipeline,
                        source,
                    },
                );
            }};
        }
        builtin!("hard-round");
        builtin!("pencil");
        builtin!("marker");
        builtin!("palette-knife");
        builtin!("charcoal");
        Ok(Self {
            brushes,
            errors: Vec::new(),
            root,
            rejected: BTreeMap::new(),
            next_scan: Instant::now(),
            pending: None,
        })
    }
    pub fn poll_deadline(&self) -> Instant {
        if self.pending.is_some() {
            Instant::now() + Duration::from_millis(100)
        } else {
            self.next_scan
        }
    }

    pub fn request_reload(&mut self) {
        self.next_scan = Instant::now();
    }

    /// Returns true when accepted brushes or diagnostics change. Never blocks on the worker.
    pub fn poll(
        &mut self,
        device: &wgpu::Device,
        layout: &wgpu::PipelineLayout,
        accept: bool,
    ) -> bool {
        let mut changed = false;
        if accept {
            if let Some(receiver) = &self.pending {
                match receiver.try_recv() {
                    Ok(reload) => {
                        changed = self.errors != reload.errors
                            || self.brushes.len() != reload.brushes.len()
                            || reload.brushes.iter().any(|(id, b)| {
                                self.brushes.get(id).is_none_or(|old| {
                                    old.manifest != b.manifest || old.source != b.source
                                })
                            });
                        self.brushes = reload.brushes;
                        self.errors = reload.errors;
                        self.rejected = reload.rejected;
                        self.pending = None;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.errors =
                            vec!["Brush reload worker stopped; keeping previous brushes".into()];
                        self.pending = None;
                        changed = true;
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                }
            }
        }
        if self.pending.is_none() && Instant::now() >= self.next_scan {
            self.next_scan = Instant::now() + Duration::from_secs(1);
            let (send, receive) = mpsc::channel();
            let device = device.clone();
            let layout = layout.clone();
            let root = self.root.clone();
            let brushes = self.brushes.clone();
            let rejected = self.rejected.clone();
            std::thread::spawn(move || {
                let _ = send.send(scan(&root, brushes, rejected, &device, &layout));
            });
            self.pending = Some(receive);
        }
        changed
    }
}

pub fn default_brush_directory() -> PathBuf {
    std::env::var_os("SKETCHPAD_BRUSH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("brushes"))
}
fn read_limited(path: &Path, limit: u64) -> Result<String, String> {
    use std::io::Read;
    let mut text = String::new();
    std::fs::File::open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .take(limit + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() as u64 > limit {
        return Err(format!("{} exceeds {limit} bytes", path.display()));
    }
    Ok(text)
}
fn scan(
    root: &Path,
    mut brushes: BTreeMap<String, ShaderBrush>,
    mut rejected: Rejections,
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
) -> Reload {
    let mut errors = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) => {
            return Reload {
                brushes,
                errors: vec![format!("{}: {error}", root.display())],
                rejected,
            }
        }
    };
    let mut paths: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    paths.sort();
    if paths.len() > MAX_PACKAGES {
        errors.push(format!(
            "Brush library supports at most {MAX_PACKAGES} packages"
        ));
        paths.truncate(MAX_PACKAGES);
    }
    rejected.retain(|path, _| paths.contains(path));
    let mut seen = HashSet::new();
    for path in paths {
        let result = (|| {
            let manifest_text = read_limited(&path.join("brush.json"), 8192)?;
            let source = read_limited(&path.join("brush.wgsl"), MAX_SOURCE_BYTES)?;
            let identity = format!("{}:{manifest_text}{source}", manifest_text.len());
            if let Some((previous, error)) = rejected.get(&path) {
                if *previous == identity {
                    return Err(error.clone());
                }
            }
            let candidate = (|| {
                let manifest = BrushManifest::parse(&manifest_text)?;
                // Directory identity prevents duplicate IDs, rename races and accidental overrides.
                if path.file_name().and_then(|s| s.to_str()) != Some(manifest.id.as_str()) {
                    return Err("directory name must match brush id".into());
                }
                if !seen.insert(manifest.id.clone()) {
                    return Err("duplicate brush id".into());
                }
                if let Some(previous) = brushes.get_mut(&manifest.id) {
                    if previous.source.as_ref() == source {
                        previous.manifest = manifest;
                        return Ok(());
                    }
                }
                if !brushes.contains_key(&manifest.id) && brushes.len() >= MAX_PACKAGES {
                    return Err(format!("Brush library already retains {MAX_PACKAGES} brushes; restart after removing unused packages"));
                }
                let pipeline = if manifest.api_version == 2 {
                    RoundMaskTarget::compile_material_brush(device, layout, &source)?
                } else {
                    RoundMaskTarget::compile_brush(device, layout, &source)?
                };
                brushes.insert(
                    manifest.id.clone(),
                    ShaderBrush {
                        manifest,
                        pipeline,
                        source: source.into(),
                    },
                );
                Ok::<_, String>(())
            })();
            if let Err(error) = &candidate {
                rejected.insert(path.clone(), (identity, error.clone()));
            } else {
                rejected.remove(&path);
            }
            candidate
        })();
        if let Err(error) = result {
            errors.push(format!("{}: {error}", path.display()));
        }
    }
    // Missing or temporarily invalid packages retain their last working version.
    Reload {
        brushes,
        errors,
        rejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manifest_rejects_invalid_contracts() {
        let valid = include_str!("../brushes/stipple/brush.json");
        assert_eq!(
            BrushManifest::parse(valid).unwrap().footprint,
            Footprint::Constant
        );
        for bad in [
            valid.replace("\"api_version\": 1", "\"api_version\": 2"),
            valid.replace("\"extent\": 1.0", "\"extent\": 20.0"),
            valid.replace("\"stipple\"", "\"../stipple\""),
            valid.replace("\"constant\"", "\"unknown\""),
        ] {
            assert!(BrushManifest::parse(&bad).is_err());
        }
    }
    #[test]
    fn constant_footprint_reserves_space_at_light_pressure() {
        let manifest = BrushManifest::parse(include_str!("../brushes/stipple/brush.json")).unwrap();
        assert_eq!(manifest.radius(512.0, 0.01, [0.0; 2]), 256.0);
        assert_eq!(
            manifest
                .recipe(StrokeMaterial::paint([0.0; 3], 1.0, 1.0).unwrap(), 512.0)
                .radius_for_pressure(0.01),
            256.0
        );
    }
}
