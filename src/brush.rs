use crate::raster::{Damage, GestureId, RasterError, RasterLayer, RectU32, TileCoord, TileEdit};
use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrushSample {
    pub position: [f32; 2],
    pub pressure: f32,
}

impl BrushSample {
    pub const fn new(position: [f32; 2], pressure: f32) -> Self {
        Self { position, pressure }
    }

    fn is_finite(self) -> bool {
        self.position[0].is_finite() && self.position[1].is_finite() && self.pressure.is_finite()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HardRoundMode {
    Paint,
    Erase,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HardRoundBrush {
    color: [f32; 3],
    diameter: f32,
    opacity: f32,
    spacing: f32,
    mode: HardRoundMode,
}

impl HardRoundBrush {
    pub fn new(
        color: [f32; 3],
        diameter: f32,
        opacity: f32,
        spacing_fraction: f32,
    ) -> Result<Self, BrushError> {
        if color
            .iter()
            .any(|channel| !channel.is_finite() || !(0.0..=1.0).contains(channel))
        {
            return Err(BrushError::InvalidColor);
        }
        if !diameter.is_finite() || diameter <= 0.0 {
            return Err(BrushError::InvalidDiameter);
        }
        if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
            return Err(BrushError::InvalidOpacity);
        }
        if !spacing_fraction.is_finite() || !(0.01..=1.0).contains(&spacing_fraction) {
            return Err(BrushError::InvalidSpacing);
        }

        Ok(Self {
            color,
            diameter,
            opacity,
            spacing: diameter * spacing_fraction,
            mode: HardRoundMode::Paint,
        })
    }

    pub fn eraser(diameter: f32, opacity: f32, spacing_fraction: f32) -> Result<Self, BrushError> {
        let mut brush = Self::new([0.0; 3], diameter, opacity, spacing_fraction)?;
        brush.mode = HardRoundMode::Erase;
        Ok(brush)
    }

    pub const fn color(self) -> [f32; 3] {
        self.color
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

    pub const fn mode(self) -> HardRoundMode {
        self.mode
    }

    pub fn radius_for_pressure(self, pressure: f32) -> f32 {
        self.diameter * 0.5 * pressure.clamp(0.0, 1.0).max(0.05)
    }

    pub fn with_diameter(mut self, diameter: f32) -> Result<Self, BrushError> {
        if !diameter.is_finite() || diameter <= 0.0 {
            return Err(BrushError::InvalidDiameter);
        }
        let spacing_fraction = self.spacing / self.diameter;
        self.diameter = diameter;
        self.spacing = diameter * spacing_fraction;
        Ok(self)
    }

    pub fn with_opacity(mut self, opacity: f32) -> Result<Self, BrushError> {
        if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
            return Err(BrushError::InvalidOpacity);
        }
        self.opacity = opacity;
        Ok(self)
    }

    pub fn with_color(mut self, color: [f32; 3]) -> Result<Self, BrushError> {
        if color
            .iter()
            .any(|channel| !channel.is_finite() || !(0.0..=1.0).contains(channel))
        {
            return Err(BrushError::InvalidColor);
        }
        self.color = color;
        Ok(self)
    }

    fn paint_dab(
        self,
        layer: &mut RasterLayer,
        gesture: GestureId,
        sample: BrushSample,
    ) -> Result<(), BrushError> {
        if !sample.is_finite() {
            return Err(BrushError::InvalidSample);
        }

        let pressure = sample.pressure.clamp(0.0, 1.0);
        if pressure == 0.0 || self.opacity == 0.0 {
            return Ok(());
        }

        let radius = self.radius_for_pressure(pressure);
        let Some(footprint) =
            RoundDabFootprint::new(layer.width(), layer.height(), sample.position, radius)
        else {
            return Ok(());
        };

        let tile_size = layer.tile_size();
        let [min_tile_x, min_tile_y, max_tile_x, max_tile_y] =
            footprint.inclusive_tile_range(tile_size);

        for tile_y in min_tile_y..=max_tile_y {
            for tile_x in min_tile_x..=max_tile_x {
                let coord = TileCoord::new(tile_x, tile_y);
                let tile_bounds = layer
                    .tile_bounds(coord)
                    .expect("coordinates derived from clipped canvas bounds are valid");
                let global_min_x = footprint.min_x.max(tile_bounds.min_x());
                let global_min_y = footprint.min_y.max(tile_bounds.min_y());
                let global_max_x = footprint.max_x.min(tile_bounds.max_x());
                let global_max_y = footprint.max_y.min(tile_bounds.max_y());
                let local_damage = RectU32::from_min_max(
                    global_min_x - tile_bounds.min_x(),
                    global_min_y - tile_bounds.min_y(),
                    global_max_x - tile_bounds.min_x(),
                    global_max_y - tile_bounds.min_y(),
                )
                .expect("the brush bounds intersect every enumerated tile");
                let tile_origin = [tile_bounds.min_x(), tile_bounds.min_y()];
                let kernel = DabKernel {
                    brush: self,
                    footprint,
                    local_damage,
                    tile_origin,
                };

                match self.mode {
                    HardRoundMode::Paint => {
                        layer.edit_tile_additive(gesture, coord, local_damage, |tile| {
                            ((), kernel.run::<false>(tile))
                        })?;
                    }
                    HardRoundMode::Erase => {
                        if !layer.tile_is_allocated(coord) {
                            continue;
                        }
                        layer.edit_tile_subtractive(gesture, coord, local_damage, |tile| {
                            ((), kernel.run::<true>(tile))
                        })?;
                    }
                }
            }
        }

        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(crate) struct RoundDabFootprint {
    position: [f32; 2],
    min_x: u32,
    min_y: u32,
    max_x: u32,
    max_y: u32,
    outer_radius: f32,
    outer_radius_squared: f32,
    inner_radius_squared: f32,
}

impl RoundDabFootprint {
    pub(crate) fn new(
        canvas_width: u32,
        canvas_height: u32,
        position: [f32; 2],
        radius: f32,
    ) -> Option<Self> {
        let fringe = 0.5;
        let outer_radius = radius + fringe;
        let min_x = (position[0] - outer_radius)
            .floor()
            .max(0.0)
            .min(canvas_width as f32) as u32;
        let min_y = (position[1] - outer_radius)
            .floor()
            .max(0.0)
            .min(canvas_height as f32) as u32;
        let max_x = (position[0] + outer_radius)
            .ceil()
            .max(0.0)
            .min(canvas_width as f32) as u32;
        let max_y = (position[1] + outer_radius)
            .ceil()
            .max(0.0)
            .min(canvas_height as f32) as u32;
        if min_x >= max_x || min_y >= max_y {
            return None;
        }
        let inner_radius = (radius - fringe).max(0.0);
        Some(Self {
            position,
            min_x,
            min_y,
            max_x,
            max_y,
            outer_radius,
            outer_radius_squared: outer_radius * outer_radius,
            inner_radius_squared: inner_radius * inner_radius,
        })
    }

    pub(crate) fn inclusive_tile_range(self, tile_size: u32) -> [u32; 4] {
        [
            self.min_x / tile_size,
            self.min_y / tile_size,
            (self.max_x - 1) / tile_size,
            (self.max_y - 1) / tile_size,
        ]
    }

    pub(crate) fn coverage(self, pixel_x: u32, pixel_y: u32) -> f32 {
        let dx = pixel_x as f32 + 0.5 - self.position[0];
        let dy = pixel_y as f32 + 0.5 - self.position[1];
        let distance_squared = dx * dx + dy * dy;
        if distance_squared >= self.outer_radius_squared {
            0.0
        } else if distance_squared <= self.inner_radius_squared {
            1.0
        } else {
            self.outer_radius - distance_squared.sqrt()
        }
    }
}

struct DabKernel {
    brush: HardRoundBrush,
    footprint: RoundDabFootprint,
    local_damage: RectU32,
    tile_origin: [u32; 2],
}

impl DabKernel {
    fn run<const ERASE: bool>(self, tile: &mut TileEdit<'_>) -> Option<RectU32> {
        let stride = tile.stride();
        let pixels = tile.pixels_mut();
        let mut changed_min_x = u32::MAX;
        let mut changed_min_y = u32::MAX;
        let mut changed_max_x = 0;
        let mut changed_max_y = 0;

        for local_y in self.local_damage.min_y()..self.local_damage.max_y() {
            let world_y = self.tile_origin[1] + local_y;
            let row_start = local_y as usize * stride;

            for local_x in self.local_damage.min_x()..self.local_damage.max_x() {
                let world_x = self.tile_origin[0] + local_x;
                let coverage = self.footprint.coverage(world_x, world_y);
                if coverage == 0.0 {
                    continue;
                }
                let source_alpha = self.brush.opacity * coverage;
                let keep_destination = 1.0 - source_alpha;
                let pixel = &mut pixels[row_start + local_x as usize];
                if ERASE && pixel.a == 0.0 {
                    continue;
                }

                if ERASE {
                    pixel.r *= keep_destination;
                    pixel.g *= keep_destination;
                    pixel.b *= keep_destination;
                    pixel.a *= keep_destination;
                    if pixel.a != 0.0 {
                        continue;
                    }
                } else {
                    pixel.r = self.brush.color[0] * source_alpha + pixel.r * keep_destination;
                    pixel.g = self.brush.color[1] * source_alpha + pixel.g * keep_destination;
                    pixel.b = self.brush.color[2] * source_alpha + pixel.b * keep_destination;
                    pixel.a = source_alpha + pixel.a * keep_destination;
                }
                changed_min_x = changed_min_x.min(local_x);
                changed_min_y = changed_min_y.min(local_y);
                changed_max_x = changed_max_x.max(local_x + 1);
                changed_max_y = changed_max_y.max(local_y + 1);
            }
        }

        RectU32::from_min_max(changed_min_x, changed_min_y, changed_max_x, changed_max_y)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum BrushError {
    InvalidColor,
    InvalidDiameter,
    InvalidOpacity,
    InvalidSpacing,
    InvalidSample,
    StrokeFinalized,
    Raster(RasterError),
}

impl fmt::Display for BrushError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidColor => write!(f, "brush color channels must be finite and in 0..=1"),
            Self::InvalidDiameter => write!(f, "brush diameter must be finite and positive"),
            Self::InvalidOpacity => write!(f, "brush opacity must be finite and in 0..=1"),
            Self::InvalidSpacing => {
                write!(f, "brush spacing fraction must be finite and in 0.01..=1")
            }
            Self::InvalidSample => write!(f, "brush samples must contain only finite values"),
            Self::StrokeFinalized => write!(f, "the stroke has already been finalized"),
            Self::Raster(error) => error.fmt(f),
        }
    }
}

impl Error for BrushError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Raster(error) => Some(error),
            _ => None,
        }
    }
}

impl From<RasterError> for BrushError {
    fn from(value: RasterError) -> Self {
        Self::Raster(value)
    }
}

pub struct HardRoundStroke {
    brush: HardRoundBrush,
    gesture: GestureId,
    resampler: DistanceResampler,
    dabs_emitted: u64,
    finalized: bool,
}

impl HardRoundStroke {
    pub fn begin(
        layer: &mut RasterLayer,
        brush: HardRoundBrush,
        sample: BrushSample,
    ) -> Result<Self, BrushError> {
        if !sample.is_finite() {
            return Err(BrushError::InvalidSample);
        }

        let gesture = layer.begin_brush_gesture(brush.diameter())?;
        if let Err(error) = brush.paint_dab(layer, gesture, sample) {
            let _ = layer.cancel_gesture(gesture);
            return Err(error);
        }

        Ok(Self {
            brush,
            gesture,
            resampler: DistanceResampler::new(sample, brush.spacing()),
            dabs_emitted: 1,
            finalized: false,
        })
    }

    pub const fn gesture_id(&self) -> GestureId {
        self.gesture
    }

    pub const fn dabs_emitted(&self) -> u64 {
        self.dabs_emitted
    }

    pub fn update(
        &mut self,
        layer: &mut RasterLayer,
        sample: BrushSample,
    ) -> Result<(), BrushError> {
        if self.finalized {
            return Err(BrushError::StrokeFinalized);
        }
        if !sample.is_finite() {
            return Err(BrushError::InvalidSample);
        }

        let brush = self.brush;
        let gesture = self.gesture;
        self.dabs_emitted += self
            .resampler
            .update(sample, |dab| brush.paint_dab(layer, gesture, dab))?;
        Ok(())
    }

    pub fn finalize(&mut self, layer: &mut RasterLayer) -> Result<(), BrushError> {
        if self.finalized {
            return Ok(());
        }
        let brush = self.brush;
        let gesture = self.gesture;
        self.dabs_emitted += self
            .resampler
            .finalize(|dab| brush.paint_dab(layer, gesture, dab))?;
        self.finalized = true;
        Ok(())
    }

    pub fn finish(mut self, layer: &mut RasterLayer) -> Result<Option<Damage>, BrushError> {
        self.finalize(layer)?;
        Ok(layer.commit_gesture(self.gesture)?)
    }

    pub fn cancel(self, layer: &mut RasterLayer) -> Result<Option<Damage>, BrushError> {
        Ok(layer.cancel_gesture(self.gesture)?)
    }
}

pub(crate) struct DistanceResampler {
    last_sample: BrushSample,
    spacing: f32,
    distance_to_next_dab: f32,
}

impl DistanceResampler {
    pub(crate) const fn new(first_sample: BrushSample, spacing: f32) -> Self {
        Self {
            last_sample: first_sample,
            spacing,
            distance_to_next_dab: spacing,
        }
    }

    pub(crate) fn update<E>(
        &mut self,
        sample: BrushSample,
        mut emit: impl FnMut(BrushSample) -> Result<(), E>,
    ) -> Result<u64, E> {
        let start = self.last_sample;
        let dx = sample.position[0] - start.position[0];
        let dy = sample.position[1] - start.position[1];
        let segment_length = (dx * dx + dy * dy).sqrt();
        if segment_length == 0.0 {
            self.last_sample = sample;
            return Ok(0);
        }

        let mut emitted = 0;
        let mut distance = self.distance_to_next_dab;
        while distance <= segment_length {
            let t = distance / segment_length;
            emit(BrushSample {
                position: [start.position[0] + dx * t, start.position[1] + dy * t],
                pressure: start.pressure + (sample.pressure - start.pressure) * t,
            })?;
            emitted += 1;
            distance += self.spacing;
        }

        self.distance_to_next_dab = distance - segment_length;
        self.last_sample = sample;
        Ok(emitted)
    }

    pub(crate) fn finalize<E>(
        &mut self,
        mut emit: impl FnMut(BrushSample) -> Result<(), E>,
    ) -> Result<u64, E> {
        let distance_since_last_dab = self.spacing - self.distance_to_next_dab;
        if distance_since_last_dab <= f32::EPSILON {
            return Ok(0);
        }
        emit(self.last_sample)?;
        self.distance_to_next_dab = self.spacing;
        Ok(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster::LinearRgba;

    fn brush() -> HardRoundBrush {
        HardRoundBrush::new([0.8, 0.1, 0.2], 20.0, 1.0, 0.2).unwrap()
    }

    fn layer() -> RasterLayer {
        RasterLayer::new(256, 256, 64).unwrap()
    }

    fn assert_layers_close(left: &RasterLayer, right: &RasterLayer) {
        for y in 0..left.height() {
            for x in 0..left.width() {
                let a = left.pixel(x, y).unwrap();
                let b = right.pixel(x, y).unwrap();
                assert!((a.r - b.r).abs() < 0.000_01, "r differs at ({x}, {y})");
                assert!((a.g - b.g).abs() < 0.000_01, "g differs at ({x}, {y})");
                assert!((a.b - b.b).abs() < 0.000_01, "b differs at ({x}, {y})");
                assert!((a.a - b.a).abs() < 0.000_01, "a differs at ({x}, {y})");
            }
        }
    }

    #[test]
    fn rejects_invalid_brush_parameters() {
        assert_eq!(
            HardRoundBrush::new([1.1, 0.0, 0.0], 20.0, 1.0, 0.2),
            Err(BrushError::InvalidColor)
        );
        assert_eq!(
            HardRoundBrush::new([1.0, 0.0, 0.0], 0.0, 1.0, 0.2),
            Err(BrushError::InvalidDiameter)
        );
        assert_eq!(
            HardRoundBrush::new([1.0, 0.0, 0.0], 20.0, 1.1, 0.2),
            Err(BrushError::InvalidOpacity)
        );
    }

    #[test]
    fn brush_adjustments_preserve_mode_and_relative_spacing() {
        let adjusted = HardRoundBrush::eraser(20.0, 0.8, 0.25)
            .unwrap()
            .with_diameter(40.0)
            .unwrap()
            .with_opacity(0.5)
            .unwrap();

        assert_eq!(adjusted.mode(), HardRoundMode::Erase);
        assert_eq!(adjusted.diameter(), 40.0);
        assert_eq!(adjusted.spacing(), 10.0);
        assert_eq!(adjusted.opacity(), 0.5);
        assert_eq!(adjusted.radius_for_pressure(0.0), 1.0);
        assert_eq!(adjusted.radius_for_pressure(0.5), 10.0);
        assert_eq!(adjusted.radius_for_pressure(1.0), 20.0);
    }

    #[test]
    fn one_dab_allocates_only_intersected_tiles() {
        let mut layer = layer();
        let stroke =
            HardRoundStroke::begin(&mut layer, brush(), BrushSample::new([63.0, 63.0], 1.0))
                .unwrap();
        let damage = stroke.finish(&mut layer).unwrap().unwrap();

        assert_eq!(damage.tiles().len(), 4);
        assert_eq!(layer.allocated_tile_count(), 4);
        assert!(layer.pixel(63, 63).unwrap().a > 0.99);
        assert_eq!(layer.undo_depth(), 1);
    }

    #[test]
    fn resampling_is_independent_of_collinear_event_batching() {
        let mut direct = layer();
        let mut direct_stroke =
            HardRoundStroke::begin(&mut direct, brush(), BrushSample::new([20.0, 80.0], 1.0))
                .unwrap();
        direct_stroke
            .update(&mut direct, BrushSample::new([220.0, 80.0], 1.0))
            .unwrap();
        direct_stroke.finish(&mut direct).unwrap();

        let mut chunked = layer();
        let mut chunked_stroke =
            HardRoundStroke::begin(&mut chunked, brush(), BrushSample::new([20.0, 80.0], 1.0))
                .unwrap();
        for x in [60.0, 100.0, 140.0, 180.0, 220.0] {
            chunked_stroke
                .update(&mut chunked, BrushSample::new([x, 80.0], 1.0))
                .unwrap();
        }
        chunked_stroke.finish(&mut chunked).unwrap();

        assert_layers_close(&direct, &chunked);
    }

    #[test]
    fn many_updates_commit_as_one_undo_transaction() {
        let mut layer = layer();
        let mut stroke =
            HardRoundStroke::begin(&mut layer, brush(), BrushSample::new([20.0, 20.0], 1.0))
                .unwrap();
        for point in [[40.0, 40.0], [80.0, 100.0], [180.0, 180.0]] {
            stroke
                .update(&mut layer, BrushSample::new(point, 1.0))
                .unwrap();
        }
        assert!(stroke.dabs_emitted() > 3);
        stroke.finish(&mut layer).unwrap();

        assert_eq!(layer.undo_depth(), 1);
        assert!(layer.allocated_tile_count() > 1);
        assert_eq!(layer.stats().content_bound_pixels_scanned, 0);
        layer.undo();
        assert_eq!(layer.allocated_tile_count(), 0);
    }

    #[test]
    fn finishing_places_a_cap_at_the_last_sample() {
        let mut layer = layer();
        let wide_spacing = HardRoundBrush::new([0.8, 0.1, 0.2], 10.0, 1.0, 1.0).unwrap();
        let mut stroke = HardRoundStroke::begin(
            &mut layer,
            wide_spacing,
            BrushSample::new([20.0, 40.0], 1.0),
        )
        .unwrap();
        stroke
            .update(&mut layer, BrushSample::new([29.0, 40.0], 1.0))
            .unwrap();
        assert_eq!(layer.pixel(29, 40).unwrap(), LinearRgba::TRANSPARENT);

        stroke.finish(&mut layer).unwrap();

        assert!(layer.pixel(29, 40).unwrap().a > 0.99);
    }

    #[test]
    fn cancellation_removes_the_active_stroke() {
        let mut layer = layer();
        let stroke =
            HardRoundStroke::begin(&mut layer, brush(), BrushSample::new([100.0, 100.0], 1.0))
                .unwrap();
        stroke.cancel(&mut layer).unwrap();

        assert_eq!(layer.allocated_tile_count(), 0);
        assert_eq!(layer.undo_depth(), 0);
    }

    #[test]
    fn pressure_changes_the_footprint() {
        let mut light = layer();
        HardRoundStroke::begin(&mut light, brush(), BrushSample::new([100.0, 100.0], 0.25))
            .unwrap()
            .finish(&mut light)
            .unwrap();

        let mut heavy = layer();
        HardRoundStroke::begin(&mut heavy, brush(), BrushSample::new([100.0, 100.0], 1.0))
            .unwrap()
            .finish(&mut heavy)
            .unwrap();

        let light_pixels = painted_pixel_count(&light);
        let heavy_pixels = painted_pixel_count(&heavy);
        assert!(light_pixels > 0);
        assert!(heavy_pixels > light_pixels * 8);
    }

    #[test]
    fn eraser_uses_destination_out_and_undo_restores_color() {
        let mut layer = layer();
        HardRoundStroke::begin(&mut layer, brush(), BrushSample::new([100.0, 100.0], 1.0))
            .unwrap()
            .finish(&mut layer)
            .unwrap();
        let painted = layer.pixel(100, 100).unwrap();
        assert!(painted.a > 0.99);

        let eraser = HardRoundBrush::eraser(20.0, 1.0, 0.2).unwrap();
        assert_eq!(eraser.mode(), HardRoundMode::Erase);
        HardRoundStroke::begin(&mut layer, eraser, BrushSample::new([100.0, 100.0], 1.0))
            .unwrap()
            .finish(&mut layer)
            .unwrap();

        assert_eq!(layer.pixel(100, 100).unwrap(), LinearRgba::TRANSPARENT);
        layer.undo();
        assert_eq!(layer.pixel(100, 100).unwrap(), painted);
    }

    #[test]
    fn erasing_empty_space_does_not_allocate_tiles_or_history() {
        let mut layer = layer();
        let eraser = HardRoundBrush::eraser(20.0, 1.0, 0.2).unwrap();
        let damage =
            HardRoundStroke::begin(&mut layer, eraser, BrushSample::new([100.0, 100.0], 1.0))
                .unwrap()
                .finish(&mut layer)
                .unwrap();

        assert!(damage.is_none());
        assert_eq!(layer.allocated_tile_count(), 0);
        assert_eq!(layer.undo_depth(), 0);
    }

    fn painted_pixel_count(layer: &RasterLayer) -> usize {
        let mut count = 0;
        for y in 0..layer.height() {
            for x in 0..layer.width() {
                if layer.pixel(x, y).unwrap() != LinearRgba::TRANSPARENT {
                    count += 1;
                }
            }
        }
        count
    }
}
