use crate::{
    brush::{BrushError, BrushSample, DistanceResampler},
    raster::{Damage, GestureId, RasterLayer, RectU32, TileCoord, TileEdit},
};

const ORIENTATION_TILT_DEAD_ZONE: f32 = 0.12;
const ORIENTATION_MOVE_DEAD_ZONE: f32 = 0.25;
const FLAT_ASPECT_MIN: f32 = 0.12;
const FLAT_ASPECT_PRESSURE: f32 = 0.18;
const FLAT_SPACING_FRACTION: f32 = 0.07;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlatBrush {
    color: [f32; 3],
    diameter: f32,
    opacity: f32,
    spacing: f32,
}

impl FlatBrush {
    pub fn new(color: [f32; 3], diameter: f32, opacity: f32) -> Result<Self, BrushError> {
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
        Ok(Self {
            color,
            diameter,
            opacity,
            spacing: diameter * FLAT_SPACING_FRACTION,
        })
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

    fn half_extents(self, pressure: f32) -> [f32; 2] {
        let pressure = pressure.clamp(0.0, 1.0);
        [
            self.diameter * 0.5 * (0.55 + pressure.sqrt() * 0.45),
            self.diameter * 0.5 * (FLAT_ASPECT_MIN + pressure * FLAT_ASPECT_PRESSURE),
        ]
    }

    fn paint_dab(
        self,
        layer: &mut RasterLayer,
        gesture: GestureId,
        sample: BrushSample,
        direction: [f32; 2],
    ) -> Result<(), BrushError> {
        if !sample.is_finite() {
            return Err(BrushError::InvalidSample);
        }
        let pressure = sample.pressure.clamp(0.0, 1.0);
        if pressure == 0.0 || self.opacity == 0.0 {
            return Ok(());
        }
        let Some(footprint) = OrientedBoxFootprint::new(
            layer.width(),
            layer.height(),
            sample.position,
            direction,
            self.half_extents(pressure),
        ) else {
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
                let local_damage = footprint
                    .local_damage(tile_bounds)
                    .expect("the flat-brush bounds intersect every enumerated tile");
                let kernel = FlatDabKernel {
                    brush: self,
                    footprint,
                    local_damage,
                    tile_origin: [tile_bounds.min_x(), tile_bounds.min_y()],
                };
                layer.edit_tile_additive(gesture, coord, local_damage, |tile| {
                    ((), kernel.run(tile))
                })?;
            }
        }
        Ok(())
    }
}

pub struct FlatStroke {
    brush: FlatBrush,
    gesture: GestureId,
    resampler: DistanceResampler,
    orientation: OrientationTracker,
    dabs_emitted: u64,
    finalized: bool,
}

impl FlatStroke {
    pub fn begin(
        layer: &mut RasterLayer,
        brush: FlatBrush,
        sample: BrushSample,
    ) -> Result<Self, BrushError> {
        if !sample.is_finite() {
            return Err(BrushError::InvalidSample);
        }
        let gesture = layer.begin_brush_gesture(brush.diameter())?;
        let mut orientation = OrientationTracker::new(sample.position);
        let direction = orientation.resolve(sample);
        if let Err(error) = brush.paint_dab(layer, gesture, sample, direction) {
            let _ = layer.cancel_gesture(gesture);
            return Err(error);
        }
        Ok(Self {
            brush,
            gesture,
            resampler: DistanceResampler::new(sample, brush.spacing()),
            orientation,
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
        let orientation = &mut self.orientation;
        self.dabs_emitted += self.resampler.update(sample, |dab| {
            let direction = orientation.resolve(dab);
            brush.paint_dab(layer, gesture, dab, direction)
        })?;
        Ok(())
    }

    pub fn finalize(&mut self, layer: &mut RasterLayer) -> Result<(), BrushError> {
        if self.finalized {
            return Ok(());
        }
        let brush = self.brush;
        let gesture = self.gesture;
        let orientation = &mut self.orientation;
        self.dabs_emitted += self.resampler.finalize(|dab| {
            let direction = orientation.resolve(dab);
            brush.paint_dab(layer, gesture, dab, direction)
        })?;
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

#[derive(Clone, Copy, Debug)]
struct OrientationTracker {
    direction: [f32; 2],
    last_position: [f32; 2],
}

impl OrientationTracker {
    const fn new(position: [f32; 2]) -> Self {
        Self {
            direction: [1.0, 0.0],
            last_position: position,
        }
    }

    fn resolve(&mut self, sample: BrushSample) -> [f32; 2] {
        let tilt_length_squared = sample.tilt[0] * sample.tilt[0] + sample.tilt[1] * sample.tilt[1];
        if tilt_length_squared >= ORIENTATION_TILT_DEAD_ZONE * ORIENTATION_TILT_DEAD_ZONE {
            let inverse_length = tilt_length_squared.sqrt().recip();
            self.direction = [
                sample.tilt[0] * inverse_length,
                sample.tilt[1] * inverse_length,
            ];
        } else {
            let dx = sample.position[0] - self.last_position[0];
            let dy = sample.position[1] - self.last_position[1];
            let movement_length_squared = dx * dx + dy * dy;
            if movement_length_squared >= ORIENTATION_MOVE_DEAD_ZONE * ORIENTATION_MOVE_DEAD_ZONE {
                let inverse_length = movement_length_squared.sqrt().recip();
                self.direction = [dx * inverse_length, dy * inverse_length];
            }
        }
        self.last_position = sample.position;
        self.direction
    }
}

#[derive(Clone, Copy, Debug)]
struct OrientedBoxFootprint {
    position: [f32; 2],
    direction: [f32; 2],
    half_extents: [f32; 2],
    min_x: u32,
    min_y: u32,
    max_x: u32,
    max_y: u32,
}

impl OrientedBoxFootprint {
    fn new(
        canvas_width: u32,
        canvas_height: u32,
        position: [f32; 2],
        direction: [f32; 2],
        half_extents: [f32; 2],
    ) -> Option<Self> {
        let fringe = 0.5;
        let bound_x =
            direction[0].abs() * half_extents[0] + direction[1].abs() * half_extents[1] + fringe;
        let bound_y =
            direction[1].abs() * half_extents[0] + direction[0].abs() * half_extents[1] + fringe;
        let min_x = (position[0] - bound_x)
            .floor()
            .max(0.0)
            .min(canvas_width as f32) as u32;
        let min_y = (position[1] - bound_y)
            .floor()
            .max(0.0)
            .min(canvas_height as f32) as u32;
        let max_x = (position[0] + bound_x)
            .ceil()
            .max(0.0)
            .min(canvas_width as f32) as u32;
        let max_y = (position[1] + bound_y)
            .ceil()
            .max(0.0)
            .min(canvas_height as f32) as u32;
        if min_x >= max_x || min_y >= max_y {
            return None;
        }
        Some(Self {
            position,
            direction,
            half_extents,
            min_x,
            min_y,
            max_x,
            max_y,
        })
    }

    fn inclusive_tile_range(self, tile_size: u32) -> [u32; 4] {
        [
            self.min_x / tile_size,
            self.min_y / tile_size,
            (self.max_x - 1) / tile_size,
            (self.max_y - 1) / tile_size,
        ]
    }

    fn local_damage(self, tile_bounds: RectU32) -> Option<RectU32> {
        let global_min_x = self.min_x.max(tile_bounds.min_x());
        let global_min_y = self.min_y.max(tile_bounds.min_y());
        let global_max_x = self.max_x.min(tile_bounds.max_x());
        let global_max_y = self.max_y.min(tile_bounds.max_y());
        RectU32::from_min_max(
            global_min_x.checked_sub(tile_bounds.min_x())?,
            global_min_y.checked_sub(tile_bounds.min_y())?,
            global_max_x.checked_sub(tile_bounds.min_x())?,
            global_max_y.checked_sub(tile_bounds.min_y())?,
        )
    }

    fn local_coordinates(self, pixel_x: u32, pixel_y: u32) -> [f32; 2] {
        let dx = pixel_x as f32 + 0.5 - self.position[0];
        let dy = pixel_y as f32 + 0.5 - self.position[1];
        [
            dx * self.direction[0] + dy * self.direction[1],
            -dx * self.direction[1] + dy * self.direction[0],
        ]
    }

    fn coverage(self, pixel_x: u32, pixel_y: u32) -> f32 {
        let local = self.local_coordinates(pixel_x, pixel_y);
        let q = [
            local[0].abs() - self.half_extents[0],
            local[1].abs() - self.half_extents[1],
        ];
        let outside = (q[0].max(0.0) * q[0].max(0.0) + q[1].max(0.0) * q[1].max(0.0)).sqrt();
        let inside = q[0].max(q[1]).min(0.0);
        (0.5 - (outside + inside)).clamp(0.0, 1.0)
    }
}

struct FlatDabKernel {
    brush: FlatBrush,
    footprint: OrientedBoxFootprint,
    local_damage: RectU32,
    tile_origin: [u32; 2],
}

impl FlatDabKernel {
    fn run(self, tile: &mut TileEdit<'_>) -> Option<RectU32> {
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
                pixel.r = self.brush.color[0] * source_alpha + pixel.r * keep_destination;
                pixel.g = self.brush.color[1] * source_alpha + pixel.g * keep_destination;
                pixel.b = self.brush.color[2] * source_alpha + pixel.b * keep_destination;
                pixel.a = source_alpha + pixel.a * keep_destination;
                changed_min_x = changed_min_x.min(local_x);
                changed_min_y = changed_min_y.min(local_y);
                changed_max_x = changed_max_x.max(local_x + 1);
                changed_max_y = changed_max_y.max(local_y + 1);
            }
        }
        RectU32::from_min_max(changed_min_x, changed_min_y, changed_max_x, changed_max_y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster::LinearRgba;

    fn layer() -> RasterLayer {
        RasterLayer::new(256, 256, 64).unwrap()
    }

    fn brush() -> FlatBrush {
        FlatBrush::new([0.2, 0.4, 0.8], 40.0, 1.0).unwrap()
    }

    fn painted_bounds(layer: &RasterLayer) -> RectU32 {
        let mut min_x = u32::MAX;
        let mut min_y = u32::MAX;
        let mut max_x = 0;
        let mut max_y = 0;
        for y in 0..layer.height() {
            for x in 0..layer.width() {
                if layer.pixel(x, y).unwrap() != LinearRgba::TRANSPARENT {
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x + 1);
                    max_y = max_y.max(y + 1);
                }
            }
        }
        RectU32::from_min_max(min_x, min_y, max_x, max_y).unwrap()
    }

    #[test]
    fn tilt_rotates_the_rectangular_contact() {
        let mut horizontal = layer();
        FlatStroke::begin(
            &mut horizontal,
            brush(),
            BrushSample::with_tilt([128.0, 128.0], 1.0, [0.8, 0.0]),
        )
        .unwrap()
        .finish(&mut horizontal)
        .unwrap();
        let horizontal = painted_bounds(&horizontal);

        let mut vertical = layer();
        FlatStroke::begin(
            &mut vertical,
            brush(),
            BrushSample::with_tilt([128.0, 128.0], 1.0, [0.0, 0.8]),
        )
        .unwrap()
        .finish(&mut vertical)
        .unwrap();
        let vertical = painted_bounds(&vertical);

        assert!(horizontal.width() > horizontal.height() * 2);
        assert!(vertical.height() > vertical.width() * 2);
        assert_eq!(horizontal.width(), vertical.height());
        assert_eq!(horizontal.height(), vertical.width());
    }

    #[test]
    fn upright_orientation_follows_motion_without_jittering_on_a_tap() {
        let mut stroke_orientation = OrientationTracker::new([10.0, 10.0]);
        assert_eq!(
            stroke_orientation.resolve(BrushSample::new([10.0, 10.0], 1.0)),
            [1.0, 0.0]
        );
        let direction = stroke_orientation.resolve(BrushSample::new([10.0, 20.0], 1.0));
        assert!(direction[0].abs() < 0.000_01);
        assert!((direction[1] - 1.0).abs() < 0.000_01);
        assert_eq!(
            stroke_orientation.resolve(BrushSample::with_tilt([10.0, 20.1], 1.0, [0.01, -0.01],)),
            direction
        );
    }

    #[test]
    fn cancellation_restores_all_flat_brush_pixels() {
        let mut layer = layer();
        let mut stroke = FlatStroke::begin(
            &mut layer,
            brush(),
            BrushSample::with_tilt([40.0, 40.0], 1.0, [0.8, 0.0]),
        )
        .unwrap();
        stroke
            .update(
                &mut layer,
                BrushSample::with_tilt([200.0, 180.0], 1.0, [0.0, 0.8]),
            )
            .unwrap();
        stroke.cancel(&mut layer).unwrap();

        assert_eq!(layer.allocated_tile_count(), 0);
        assert_eq!(layer.undo_depth(), 0);
    }

    #[test]
    fn tilt_is_interpolated_by_the_shared_distance_resampler() {
        let first = BrushSample::with_tilt([0.0, 0.0], 0.2, [0.0, -0.5]);
        let mut resampler = DistanceResampler::new(first, 5.0);
        let mut emitted = Vec::new();
        resampler
            .update(
                BrushSample::with_tilt([10.0, 0.0], 1.0, [1.0, 0.5]),
                |sample| {
                    emitted.push(sample);
                    Ok::<(), ()>(())
                },
            )
            .unwrap();

        assert_eq!(emitted.len(), 2);
        assert_eq!(
            emitted[0],
            BrushSample::with_tilt([5.0, 0.0], 0.6, [0.5, 0.0])
        );
        assert_eq!(
            emitted[1],
            BrushSample::with_tilt([10.0, 0.0], 1.0, [1.0, 0.5])
        );
    }
}
