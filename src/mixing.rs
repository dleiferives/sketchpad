use crate::{
    brush::{BrushSample, DistanceResampler, RoundDabFootprint},
    raster::{
        Damage, GestureId, LinearRgba, RasterError, RasterLayer, RectU32, TileCoord, TileEdit,
    },
};
use std::{
    collections::{hash_map::Entry, HashMap},
    error::Error,
    fmt, mem,
};

pub const MIXING_RECIPE_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LinearRgb {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

impl LinearRgb {
    pub const fn new(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b }
    }

    fn is_valid(self) -> bool {
        [self.r, self.g, self.b]
            .into_iter()
            .all(|channel| channel.is_finite() && (0.0..=1.0).contains(&channel))
    }

    fn lerp(self, destination: Self, amount: f32) -> Self {
        if amount == 0.0 || self == destination {
            return self;
        }
        if amount == 1.0 {
            return destination;
        }
        let keep = 1.0 - amount;
        Self {
            r: self.r * keep + destination.r * amount,
            g: self.g * keep + destination.g * amount,
            b: self.b * keep + destination.b * amount,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SampledColor {
    pub color: LinearRgb,
    pub strength: f32,
}

impl SampledColor {
    pub fn new(color: LinearRgb, strength: f32) -> Result<Self, MixingError> {
        if !color.is_valid() {
            return Err(MixingError::InvalidColor);
        }
        if !unit_value(strength) {
            return Err(MixingError::InvalidStrength);
        }
        Ok(Self { color, strength })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MixingRecipeV1 {
    foreground: LinearRgb,
    pickup: f32,
    color_rate: f32,
}

impl MixingRecipeV1 {
    pub fn new(foreground: LinearRgb, pickup: f32, color_rate: f32) -> Result<Self, MixingError> {
        if !foreground.is_valid() {
            return Err(MixingError::InvalidColor);
        }
        if !unit_value(pickup) {
            return Err(MixingError::InvalidPickup);
        }
        if !unit_value(color_rate) {
            return Err(MixingError::InvalidColorRate);
        }
        Ok(Self {
            foreground,
            pickup,
            color_rate,
        })
    }

    pub const fn version(self) -> u32 {
        MIXING_RECIPE_VERSION
    }

    pub const fn foreground(self) -> LinearRgb {
        self.foreground
    }

    pub const fn pickup(self) -> f32 {
        self.pickup
    }

    pub const fn color_rate(self) -> f32 {
        self.color_rate
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UniformReservoir {
    color: LinearRgb,
    dabs_observed: u64,
}

impl UniformReservoir {
    pub const fn new(recipe: MixingRecipeV1) -> Self {
        Self {
            color: recipe.foreground,
            dabs_observed: 0,
        }
    }

    pub const fn color(self) -> LinearRgb {
        self.color
    }

    pub const fn dabs_observed(self) -> u64 {
        self.dabs_observed
    }

    pub fn advance(&mut self, recipe: MixingRecipeV1, sample: Option<SampledColor>) -> LinearRgb {
        if let Some(sample) = sample {
            self.color = self
                .color
                .lerp(sample.color, recipe.pickup * sample.strength);
        }
        self.color = self.color.lerp(recipe.foreground, recipe.color_rate);
        self.dabs_observed = self.dabs_observed.saturating_add(1);
        self.color
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MixingStats {
    pub dabs: u64,
    pub sampled_tiles: u64,
    pub sampled_pixels: u64,
    pub deposited_pixels: u64,
    pub snapshot_tiles: u64,
    pub snapshot_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MixingBrushV1 {
    recipe: MixingRecipeV1,
    diameter: f32,
    opacity: f32,
    spacing: f32,
}

impl MixingBrushV1 {
    pub fn new(
        recipe: MixingRecipeV1,
        diameter: f32,
        opacity: f32,
        spacing_fraction: f32,
    ) -> Result<Self, MixingError> {
        if !diameter.is_finite() || diameter <= 0.0 {
            return Err(MixingError::InvalidDiameter);
        }
        if !unit_value(opacity) {
            return Err(MixingError::InvalidOpacity);
        }
        if !spacing_fraction.is_finite() || !(0.01..=1.0).contains(&spacing_fraction) {
            return Err(MixingError::InvalidSpacing);
        }
        Ok(Self {
            recipe,
            diameter,
            opacity,
            spacing: diameter * spacing_fraction,
        })
    }

    pub const fn recipe(self) -> MixingRecipeV1 {
        self.recipe
    }

    pub const fn diameter(self) -> f32 {
        self.diameter
    }

    pub const fn opacity(self) -> f32 {
        self.opacity
    }

    pub const fn spacing(self) -> f32 {
        self.spacing
    }

    pub fn radius_for_pressure(self, pressure: f32) -> f32 {
        self.diameter * 0.5 * pressure.clamp(0.0, 1.0).max(0.05)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum MixingError {
    InvalidColor,
    InvalidStrength,
    InvalidPickup,
    InvalidColorRate,
    InvalidDiameter,
    InvalidOpacity,
    InvalidSpacing,
    InvalidSample,
    StrokeFinalized,
    Raster(RasterError),
}

impl fmt::Display for MixingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidColor => {
                write!(formatter, "mixing colors must be finite and in 0..=1")
            }
            Self::InvalidStrength => {
                write!(formatter, "sample strength must be finite and in 0..=1")
            }
            Self::InvalidPickup => write!(formatter, "pickup must be finite and in 0..=1"),
            Self::InvalidColorRate => {
                write!(formatter, "color rate must be finite and in 0..=1")
            }
            Self::InvalidDiameter => {
                write!(
                    formatter,
                    "mixing-brush diameter must be finite and positive"
                )
            }
            Self::InvalidOpacity => {
                write!(
                    formatter,
                    "mixing-brush opacity must be finite and in 0..=1"
                )
            }
            Self::InvalidSpacing => {
                write!(
                    formatter,
                    "mixing-brush spacing fraction must be finite and in 0.01..=1"
                )
            }
            Self::InvalidSample => {
                write!(
                    formatter,
                    "mixing-brush samples must contain only finite values"
                )
            }
            Self::StrokeFinalized => write!(formatter, "the mixing stroke has already finalized"),
            Self::Raster(error) => error.fmt(formatter),
        }
    }
}

impl Error for MixingError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Raster(error) => Some(error),
            _ => None,
        }
    }
}

impl From<RasterError> for MixingError {
    fn from(value: RasterError) -> Self {
        Self::Raster(value)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MixingStrokeResult {
    pub damage: Option<Damage>,
    pub stats: MixingStats,
    pub final_color: LinearRgb,
}

enum PickupTile {
    Transparent,
    Pixels(Box<[LinearRgba]>),
}

pub struct MixingStrokeV1 {
    brush: MixingBrushV1,
    gesture: GestureId,
    resampler: DistanceResampler,
    reservoir: UniformReservoir,
    pickup_tiles: HashMap<TileCoord, PickupTile>,
    stats: MixingStats,
    finalized: bool,
}

impl MixingStrokeV1 {
    pub fn begin(
        layer: &mut RasterLayer,
        brush: MixingBrushV1,
        sample: BrushSample,
    ) -> Result<Self, MixingError> {
        if !sample.is_finite() {
            return Err(MixingError::InvalidSample);
        }

        let gesture = layer.begin_brush_gesture(brush.diameter())?;
        let mut stroke = Self {
            brush,
            gesture,
            resampler: DistanceResampler::new(sample, brush.spacing()),
            reservoir: UniformReservoir::new(brush.recipe()),
            pickup_tiles: HashMap::new(),
            stats: MixingStats::default(),
            finalized: false,
        };
        if let Err(error) = stroke.paint_dab(layer, sample) {
            let _ = layer.cancel_gesture(gesture);
            return Err(error);
        }
        Ok(stroke)
    }

    pub const fn gesture_id(&self) -> GestureId {
        self.gesture
    }

    pub const fn stats(&self) -> MixingStats {
        self.stats
    }

    pub const fn held_color(&self) -> LinearRgb {
        self.reservoir.color()
    }

    pub fn update(
        &mut self,
        layer: &mut RasterLayer,
        sample: BrushSample,
    ) -> Result<(), MixingError> {
        if self.finalized {
            return Err(MixingError::StrokeFinalized);
        }
        if !sample.is_finite() {
            return Err(MixingError::InvalidSample);
        }

        let Self {
            brush,
            gesture,
            resampler,
            reservoir,
            pickup_tiles,
            stats,
            ..
        } = self;
        let brush = *brush;
        let gesture = *gesture;
        resampler.update(sample, |dab| {
            paint_mixing_dab(layer, brush, gesture, reservoir, pickup_tiles, stats, dab)
        })?;
        Ok(())
    }

    pub fn finalize(&mut self, layer: &mut RasterLayer) -> Result<(), MixingError> {
        if self.finalized {
            return Ok(());
        }
        let Self {
            brush,
            gesture,
            resampler,
            reservoir,
            pickup_tiles,
            stats,
            ..
        } = self;
        let brush = *brush;
        let gesture = *gesture;
        resampler.finalize(|dab| {
            paint_mixing_dab(layer, brush, gesture, reservoir, pickup_tiles, stats, dab)
        })?;
        self.finalized = true;
        Ok(())
    }

    pub fn finish(mut self, layer: &mut RasterLayer) -> Result<MixingStrokeResult, MixingError> {
        self.finalize(layer)?;
        let damage = layer.commit_gesture(self.gesture)?;
        Ok(MixingStrokeResult {
            damage,
            stats: self.stats,
            final_color: self.reservoir.color(),
        })
    }

    pub fn cancel(self, layer: &mut RasterLayer) -> Result<Option<Damage>, MixingError> {
        Ok(layer.cancel_gesture(self.gesture)?)
    }

    fn paint_dab(
        &mut self,
        layer: &mut RasterLayer,
        sample: BrushSample,
    ) -> Result<(), MixingError> {
        paint_mixing_dab(
            layer,
            self.brush,
            self.gesture,
            &mut self.reservoir,
            &mut self.pickup_tiles,
            &mut self.stats,
            sample,
        )
    }
}

fn paint_mixing_dab(
    layer: &mut RasterLayer,
    brush: MixingBrushV1,
    gesture: GestureId,
    reservoir: &mut UniformReservoir,
    pickup_tiles: &mut HashMap<TileCoord, PickupTile>,
    stats: &mut MixingStats,
    sample: BrushSample,
) -> Result<(), MixingError> {
    let pressure = sample.pressure.clamp(0.0, 1.0);
    if pressure == 0.0 || brush.opacity() == 0.0 {
        return Ok(());
    }

    let radius = brush.radius_for_pressure(pressure);
    let Some(footprint) =
        RoundDabFootprint::new(layer.width(), layer.height(), sample.position, radius)
    else {
        return Ok(());
    };
    let sampled = sample_stable_footprint(layer, footprint, pickup_tiles, stats);
    let held = reservoir.advance(brush.recipe(), sampled);
    stats.dabs = stats.dabs.saturating_add(1);
    deposit_footprint(layer, gesture, brush.opacity(), held, footprint, stats)?;
    Ok(())
}

fn sample_stable_footprint(
    layer: &RasterLayer,
    footprint: RoundDabFootprint,
    pickup_tiles: &mut HashMap<TileCoord, PickupTile>,
    stats: &mut MixingStats,
) -> Option<SampledColor> {
    let tile_size = layer.tile_size();
    let [min_tile_x, min_tile_y, max_tile_x, max_tile_y] =
        footprint.inclusive_tile_range(tile_size);
    let mut coverage_sum = 0.0_f32;
    let mut alpha_sum = 0.0_f32;
    let mut premultiplied_sum = LinearRgb::new(0.0, 0.0, 0.0);

    for tile_y in min_tile_y..=max_tile_y {
        for tile_x in min_tile_x..=max_tile_x {
            let coord = TileCoord::new(tile_x, tile_y);
            let tile_bounds = layer
                .tile_bounds(coord)
                .expect("coordinates derived from clipped canvas bounds are valid");
            let local_damage = footprint
                .local_damage(tile_bounds)
                .expect("the footprint intersects every enumerated tile");
            let snapshot = match pickup_tiles.entry(coord) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    stats.snapshot_tiles = stats.snapshot_tiles.saturating_add(1);
                    let snapshot = match layer.tile(coord) {
                        Some(tile) => {
                            let pixels = tile.pixels().to_vec().into_boxed_slice();
                            stats.snapshot_bytes = stats.snapshot_bytes.saturating_add(
                                (pixels.len() as u64)
                                    .saturating_mul(mem::size_of::<LinearRgba>() as u64),
                            );
                            PickupTile::Pixels(pixels)
                        }
                        None => PickupTile::Transparent,
                    };
                    entry.insert(snapshot)
                }
            };

            stats.sampled_tiles = stats.sampled_tiles.saturating_add(1);
            let tile_origin = [tile_bounds.min_x(), tile_bounds.min_y()];
            let snapshot_pixels = match &*snapshot {
                PickupTile::Transparent => None,
                PickupTile::Pixels(pixels) => Some(pixels.as_ref()),
            };
            for local_y in local_damage.min_y()..local_damage.max_y() {
                let world_y = tile_origin[1] + local_y;
                let row_start = local_y as usize * tile_size as usize;
                for local_x in local_damage.min_x()..local_damage.max_x() {
                    let world_x = tile_origin[0] + local_x;
                    let coverage = footprint.coverage(world_x, world_y);
                    if coverage == 0.0 {
                        continue;
                    }
                    stats.sampled_pixels = stats.sampled_pixels.saturating_add(1);
                    coverage_sum += coverage;
                    if let Some(pixels) = snapshot_pixels {
                        let pixel = pixels[row_start + local_x as usize];
                        alpha_sum += pixel.a * coverage;
                        premultiplied_sum.r += pixel.r * coverage;
                        premultiplied_sum.g += pixel.g * coverage;
                        premultiplied_sum.b += pixel.b * coverage;
                    }
                }
            }
        }
    }

    if alpha_sum <= f32::EPSILON || coverage_sum <= f32::EPSILON {
        return None;
    }
    let inverse_alpha = alpha_sum.recip();
    Some(SampledColor {
        color: LinearRgb::new(
            (premultiplied_sum.r * inverse_alpha).clamp(0.0, 1.0),
            (premultiplied_sum.g * inverse_alpha).clamp(0.0, 1.0),
            (premultiplied_sum.b * inverse_alpha).clamp(0.0, 1.0),
        ),
        strength: (alpha_sum / coverage_sum).clamp(0.0, 1.0),
    })
}

fn deposit_footprint(
    layer: &mut RasterLayer,
    gesture: GestureId,
    opacity: f32,
    color: LinearRgb,
    footprint: RoundDabFootprint,
    stats: &mut MixingStats,
) -> Result<(), RasterError> {
    let tile_size = layer.tile_size();
    let [min_tile_x, min_tile_y, max_tile_x, max_tile_y] =
        footprint.inclusive_tile_range(tile_size);
    for tile_y in min_tile_y..=max_tile_y {
        for tile_x in min_tile_x..=max_tile_x {
            let coord = TileCoord::new(tile_x, tile_y);
            let tile_bounds = layer
                .tile_bounds(coord)
                .expect("coordinates derived from clipped canvas bounds are valid");
            let local_damage = footprint
                .local_damage(tile_bounds)
                .expect("the footprint intersects every enumerated tile");
            let tile_origin = [tile_bounds.min_x(), tile_bounds.min_y()];
            let deposited_pixels =
                layer.edit_tile_additive(gesture, coord, local_damage, |tile| {
                    deposit_tile(tile, local_damage, tile_origin, footprint, opacity, color)
                })?;
            stats.deposited_pixels = stats.deposited_pixels.saturating_add(deposited_pixels);
        }
    }
    Ok(())
}

fn deposit_tile(
    tile: &mut TileEdit<'_>,
    local_damage: RectU32,
    tile_origin: [u32; 2],
    footprint: RoundDabFootprint,
    opacity: f32,
    color: LinearRgb,
) -> (u64, Option<RectU32>) {
    let stride = tile.stride();
    let pixels = tile.pixels_mut();
    let mut changed_min_x = u32::MAX;
    let mut changed_min_y = u32::MAX;
    let mut changed_max_x = 0;
    let mut changed_max_y = 0;
    let mut deposited_pixels = 0_u64;

    for local_y in local_damage.min_y()..local_damage.max_y() {
        let world_y = tile_origin[1] + local_y;
        let row_start = local_y as usize * stride;
        for local_x in local_damage.min_x()..local_damage.max_x() {
            let world_x = tile_origin[0] + local_x;
            let coverage = footprint.coverage(world_x, world_y);
            if coverage == 0.0 {
                continue;
            }
            let source_alpha = opacity * coverage;
            let keep_destination = 1.0 - source_alpha;
            let pixel = &mut pixels[row_start + local_x as usize];
            pixel.r = color.r * source_alpha + pixel.r * keep_destination;
            pixel.g = color.g * source_alpha + pixel.g * keep_destination;
            pixel.b = color.b * source_alpha + pixel.b * keep_destination;
            pixel.a = source_alpha + pixel.a * keep_destination;
            deposited_pixels += 1;
            changed_min_x = changed_min_x.min(local_x);
            changed_min_y = changed_min_y.min(local_y);
            changed_max_x = changed_max_x.max(local_x + 1);
            changed_max_y = changed_max_y.max(local_y + 1);
        }
    }
    (
        deposited_pixels,
        RectU32::from_min_max(changed_min_x, changed_min_y, changed_max_x, changed_max_y),
    )
}

fn unit_value(value: f32) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::{HardRoundBrush, HardRoundStroke};

    const BLUE: LinearRgba = LinearRgba::premultiplied(0.0, 0.0, 1.0, 1.0);

    fn recipe(pickup: f32, color_rate: f32) -> MixingRecipeV1 {
        MixingRecipeV1::new(LinearRgb::new(1.0, 0.0, 0.0), pickup, color_rate).unwrap()
    }

    fn mixing_brush(foreground: LinearRgb, pickup: f32, color_rate: f32) -> MixingBrushV1 {
        MixingBrushV1::new(
            MixingRecipeV1::new(foreground, pickup, color_rate).unwrap(),
            20.0,
            0.7,
            0.2,
        )
        .unwrap()
    }

    fn layer() -> RasterLayer {
        RasterLayer::new(256, 256, 64).unwrap()
    }

    fn fill_tile(layer: &mut RasterLayer, coord: TileCoord, color: LinearRgba) {
        let bounds = layer.tile_bounds(coord).unwrap();
        let local_damage = RectU32::from_xywh(0, 0, bounds.width(), bounds.height()).unwrap();
        let gesture = layer.begin_gesture().unwrap();
        layer
            .edit_tile_additive(gesture, coord, local_damage, |tile| {
                let stride = tile.stride();
                for y in 0..local_damage.height() {
                    let row_start = y as usize * stride;
                    tile.pixels_mut()[row_start..row_start + local_damage.width() as usize]
                        .fill(color);
                }
                ((), Some(local_damage))
            })
            .unwrap();
        layer.commit_gesture(gesture).unwrap();
    }

    fn filled_layer(color: LinearRgba) -> RasterLayer {
        let mut layer = layer();
        let [tiles_wide, tiles_high] = layer.tile_grid_extent();
        for tile_y in 0..tiles_high {
            for tile_x in 0..tiles_wide {
                fill_tile(&mut layer, TileCoord::new(tile_x, tile_y), color);
            }
        }
        layer.clear_history();
        layer
    }

    fn assert_layers_exact(left: &RasterLayer, right: &RasterLayer) {
        assert_eq!(left.width(), right.width());
        assert_eq!(left.height(), right.height());
        for y in 0..left.height() {
            for x in 0..left.width() {
                assert_eq!(
                    left.pixel(x, y),
                    right.pixel(x, y),
                    "pixel differs at ({x}, {y})"
                );
            }
        }
    }

    fn assert_layers_close(left: &RasterLayer, right: &RasterLayer) {
        for y in 0..left.height() {
            for x in 0..left.width() {
                let left = left.pixel(x, y).unwrap();
                let right = right.pixel(x, y).unwrap();
                assert!((left.r - right.r).abs() < 1.0e-5, "r at ({x}, {y})");
                assert!((left.g - right.g).abs() < 1.0e-5, "g at ({x}, {y})");
                assert!((left.b - right.b).abs() < 1.0e-5, "b at ({x}, {y})");
                assert!((left.a - right.a).abs() < 1.0e-5, "a at ({x}, {y})");
            }
        }
    }

    #[test]
    fn recipe_is_versioned_and_rejects_invalid_state() {
        assert_eq!(recipe(0.5, 0.25).version(), 1);
        assert_eq!(
            MixingRecipeV1::new(LinearRgb::new(f32::NAN, 0.0, 0.0), 0.5, 0.5),
            Err(MixingError::InvalidColor)
        );
        assert_eq!(
            MixingRecipeV1::new(LinearRgb::new(1.0, 0.0, 0.0), -0.1, 0.5),
            Err(MixingError::InvalidPickup)
        );
        assert_eq!(
            MixingBrushV1::new(recipe(0.5, 0.5), 0.0, 1.0, 0.2),
            Err(MixingError::InvalidDiameter)
        );
    }

    #[test]
    fn transparent_sampling_leaves_the_reservoir_uncontaminated() {
        let recipe = recipe(1.0, 0.0);
        let mut reservoir = UniformReservoir::new(recipe);
        assert_eq!(reservoir.advance(recipe, None), recipe.foreground());
        assert_eq!(reservoir.dabs_observed(), 1);
    }

    #[test]
    fn pickup_and_fresh_color_rate_are_separate_ordered_operations() {
        let recipe = recipe(0.5, 0.25);
        let blue = SampledColor::new(LinearRgb::new(0.0, 0.0, 1.0), 1.0).unwrap();
        let mut reservoir = UniformReservoir::new(recipe);

        let result = reservoir.advance(recipe, Some(blue));

        assert_eq!(result, LinearRgb::new(0.625, 0.0, 0.375));
    }

    #[test]
    fn partial_sample_strength_scales_pickup_without_changing_color_rate() {
        let recipe = recipe(0.8, 0.0);
        let blue = SampledColor::new(LinearRgb::new(0.0, 0.0, 1.0), 0.25).unwrap();
        let mut reservoir = UniformReservoir::new(recipe);

        assert_eq!(
            reservoir.advance(recipe, Some(blue)),
            LinearRgb::new(0.8, 0.0, 0.2)
        );
    }

    #[test]
    fn saved_control_sequence_matches_reference_results() {
        let recipe = recipe(0.4, 0.1);
        let mut reservoir = UniformReservoir::new(recipe);
        let sequence = [
            Some(SampledColor::new(LinearRgb::new(0.0, 0.0, 1.0), 1.0).unwrap()),
            Some(SampledColor::new(LinearRgb::new(0.0, 1.0, 0.0), 0.5).unwrap()),
            None,
        ];
        let observed: Vec<_> = sequence
            .into_iter()
            .map(|sample| reservoir.advance(recipe, sample))
            .collect();

        let expected = [
            LinearRgb::new(0.64, 0.0, 0.36),
            LinearRgb::new(0.560_8, 0.18, 0.259_2),
            LinearRgb::new(0.604_72, 0.162, 0.233_28),
        ];
        for (actual, expected) in observed.into_iter().zip(expected) {
            assert!((actual.r - expected.r).abs() < 1.0e-6);
            assert!((actual.g - expected.g).abs() < 1.0e-6);
            assert!((actual.b - expected.b).abs() < 1.0e-6);
        }
        assert!(std::mem::size_of::<UniformReservoir>() <= 24);
    }

    #[test]
    fn transparent_pickup_is_pixel_exact_to_the_hard_round_control() {
        let foreground = LinearRgb::new(0.8, 0.1, 0.2);
        let samples = [
            BrushSample::new([20.0, 80.0], 1.0),
            BrushSample::new([100.0, 80.0], 0.7),
            BrushSample::new([220.0, 80.0], 1.0),
        ];

        let mut hard_layer = layer();
        let hard = HardRoundBrush::new([foreground.r, foreground.g, foreground.b], 20.0, 0.7, 0.2)
            .unwrap();
        let mut hard_stroke = HardRoundStroke::begin(&mut hard_layer, hard, samples[0]).unwrap();
        for sample in &samples[1..] {
            hard_stroke.update(&mut hard_layer, *sample).unwrap();
        }
        hard_stroke.finish(&mut hard_layer).unwrap();

        let mut mixing_layer = layer();
        let brush = mixing_brush(foreground, 1.0, 0.25);
        let mut mixing_stroke =
            MixingStrokeV1::begin(&mut mixing_layer, brush, samples[0]).unwrap();
        for sample in &samples[1..] {
            mixing_stroke.update(&mut mixing_layer, *sample).unwrap();
        }
        let result = mixing_stroke.finish(&mut mixing_layer).unwrap();

        assert_layers_exact(&hard_layer, &mixing_layer);
        assert_eq!(result.final_color, foreground);
        assert_eq!(result.stats.snapshot_bytes, 0);
        assert_eq!(result.stats.sampled_pixels, result.stats.deposited_pixels);
    }

    #[test]
    fn stable_pickup_uses_pre_stroke_pixels_across_overlapping_dabs() {
        let mut layer = filled_layer(BLUE);
        let brush = MixingBrushV1::new(recipe(0.5, 0.25), 20.0, 1.0, 1.0).unwrap();
        let mut stroke =
            MixingStrokeV1::begin(&mut layer, brush, BrushSample::new([50.0, 50.0], 1.0)).unwrap();
        stroke
            .update(&mut layer, BrushSample::new([90.0, 50.0], 1.0))
            .unwrap();
        let result = stroke.finish(&mut layer).unwrap();

        assert_eq!(result.stats.dabs, 3);
        let expected = LinearRgb::new(0.431_640_63, 0.0, 0.568_359_4);
        assert!((result.final_color.r - expected.r).abs() < 1.0e-6);
        assert!((result.final_color.g - expected.g).abs() < 1.0e-6);
        assert!((result.final_color.b - expected.b).abs() < 1.0e-6);
    }

    #[test]
    fn snapshot_cost_counts_only_preexisting_tile_payloads() {
        let mut layer = RasterLayer::new(128, 128, 64).unwrap();
        fill_tile(&mut layer, TileCoord::new(0, 0), BLUE);
        layer.clear_history();
        let brush = mixing_brush(LinearRgb::new(1.0, 0.0, 0.0), 0.5, 0.1);

        let result = MixingStrokeV1::begin(&mut layer, brush, BrushSample::new([63.0, 63.0], 1.0))
            .unwrap()
            .finish(&mut layer)
            .unwrap();

        assert_eq!(result.stats.dabs, 1);
        assert_eq!(result.stats.snapshot_tiles, 4);
        assert_eq!(
            result.stats.snapshot_bytes,
            64 * 64 * mem::size_of::<LinearRgba>() as u64
        );
        assert_eq!(result.stats.sampled_tiles, 4);
        assert_eq!(result.stats.sampled_pixels, result.stats.deposited_pixels);
    }

    #[test]
    fn mixing_is_independent_of_collinear_event_batching() {
        let brush = mixing_brush(LinearRgb::new(1.0, 0.0, 0.0), 0.65, 0.08);
        let mut direct = filled_layer(BLUE);
        let mut direct_stroke =
            MixingStrokeV1::begin(&mut direct, brush, BrushSample::new([20.0, 80.0], 1.0)).unwrap();
        direct_stroke
            .update(&mut direct, BrushSample::new([220.0, 80.0], 1.0))
            .unwrap();
        let direct_result = direct_stroke.finish(&mut direct).unwrap();

        let mut chunked = filled_layer(BLUE);
        let mut chunked_stroke =
            MixingStrokeV1::begin(&mut chunked, brush, BrushSample::new([20.0, 80.0], 1.0))
                .unwrap();
        for x in [60.0, 100.0, 140.0, 180.0, 220.0] {
            chunked_stroke
                .update(&mut chunked, BrushSample::new([x, 80.0], 1.0))
                .unwrap();
        }
        let chunked_result = chunked_stroke.finish(&mut chunked).unwrap();

        assert_layers_close(&direct, &chunked);
        assert_eq!(direct_result.final_color, chunked_result.final_color);
        assert_eq!(direct_result.stats, chunked_result.stats);
    }

    #[test]
    fn cancellation_and_undo_restore_exact_pre_stroke_pixels() {
        let brush = mixing_brush(LinearRgb::new(1.0, 0.0, 0.0), 0.7, 0.05);

        let mut cancelled = filled_layer(BLUE);
        let original = filled_layer(BLUE);
        MixingStrokeV1::begin(&mut cancelled, brush, BrushSample::new([80.0, 80.0], 1.0))
            .unwrap()
            .cancel(&mut cancelled)
            .unwrap();
        assert_layers_exact(&cancelled, &original);
        assert_eq!(cancelled.undo_depth(), 0);

        let mut undone = filled_layer(BLUE);
        MixingStrokeV1::begin(&mut undone, brush, BrushSample::new([80.0, 80.0], 1.0))
            .unwrap()
            .finish(&mut undone)
            .unwrap();
        assert_eq!(undone.undo_depth(), 1);
        undone.undo();
        assert_layers_exact(&undone, &original);
    }
}
