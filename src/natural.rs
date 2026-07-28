use crate::{
    brush::{BrushError, BrushSample, DistanceResampler},
    raster::{Damage, GestureId, RasterLayer, RectU32, TileCoord, TileEdit},
};

const ORIENTATION_TILT_DEAD_ZONE: f32 = 0.12;
const ORIENTATION_MOVE_DEAD_ZONE: f32 = 0.25;
const FLAT_ASPECT_MIN: f32 = 0.12;
const FLAT_ASPECT_PRESSURE: f32 = 0.18;
const FLAT_SPACING_FRACTION: f32 = 0.07;
const PENCIL_SPACING_FRACTION: f32 = 0.035;
const PENCIL_GRAIN_SEED: u32 = 0x91e1_0da5;
const KNIFE_LANE_COUNT: usize = 12;
const KNIFE_SPACING_FRACTION: f32 = 0.06;
const BRISTLE_COUNT: usize = 24;
const BRISTLE_SPACING_FRACTION: f32 = 0.035;
const BRISTLE_CONTACT_FRACTION: f32 = 0.42;

pub fn contact_direction_from_tilt(tilt: [f32; 2]) -> Option<[f32; 2]> {
    let length_squared = tilt[0] * tilt[0] + tilt[1] * tilt[1];
    if !length_squared.is_finite()
        || length_squared < ORIENTATION_TILT_DEAD_ZONE * ORIENTATION_TILT_DEAD_ZONE
    {
        return None;
    }
    let inverse_length = length_squared.sqrt().recip();
    Some([tilt[1] * inverse_length, -tilt[0] * inverse_length])
}

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

    pub fn contact_half_extents(self, pressure: f32) -> [f32; 2] {
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
            self.contact_half_extents(pressure),
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PencilBrush {
    color: [f32; 3],
    diameter: f32,
    opacity: f32,
    spacing: f32,
    grain_seed: u32,
}

impl PencilBrush {
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
            spacing: (diameter * PENCIL_SPACING_FRACTION).max(0.35),
            grain_seed: PENCIL_GRAIN_SEED,
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

    fn contact(self, sample: BrushSample) -> PencilContact {
        let pressure = sample.pressure.clamp(0.0, 1.0);
        let tilt_length =
            (sample.tilt[0] * sample.tilt[0] + sample.tilt[1] * sample.tilt[1]).sqrt();
        let side = ((tilt_length - 0.06) / 0.84).clamp(0.0, 1.0);
        let pressure_scale = 0.4 + 0.6 * pressure.sqrt();
        PencilContact {
            half_extents: [
                self.diameter * 0.5 * (0.16 + 0.84 * side) * pressure_scale,
                self.diameter * 0.5 * (0.12 + 0.24 * side) * pressure_scale,
            ],
            transfer: self.opacity * (0.06 + 0.38 * pressure),
        }
    }

    pub fn contact_half_extents(self, pressure: f32, tilt: [f32; 2]) -> [f32; 2] {
        self.contact(BrushSample::with_tilt([0.0, 0.0], pressure, tilt))
            .half_extents
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
        if sample.pressure <= 0.0 || self.opacity == 0.0 {
            return Ok(());
        }
        let contact = self.contact(sample);
        let Some(footprint) = OrientedEllipseFootprint::new(
            layer.width(),
            layer.height(),
            sample.position,
            direction,
            contact.half_extents,
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
                    .expect("the pencil bounds intersect every enumerated tile");
                let kernel = PencilDabKernel {
                    brush: self,
                    footprint,
                    transfer: contact.transfer,
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

#[derive(Clone, Copy)]
struct PencilContact {
    half_extents: [f32; 2],
    transfer: f32,
}

pub struct PencilStroke {
    brush: PencilBrush,
    state: OrientedStrokeState,
}

impl PencilStroke {
    pub fn begin(
        layer: &mut RasterLayer,
        brush: PencilBrush,
        sample: BrushSample,
    ) -> Result<Self, BrushError> {
        if !sample.is_finite() {
            return Err(BrushError::InvalidSample);
        }
        let gesture = layer.begin_brush_gesture(brush.diameter())?;
        let mut state = OrientedStrokeState::new(gesture, sample, brush.spacing());
        let direction = state.orientation.resolve(sample);
        if let Err(error) = brush.paint_dab(layer, gesture, sample, direction) {
            let _ = layer.cancel_gesture(gesture);
            return Err(error);
        }
        Ok(Self { brush, state })
    }

    pub const fn gesture_id(&self) -> GestureId {
        self.state.gesture
    }

    pub const fn dabs_emitted(&self) -> u64 {
        self.state.dabs_emitted
    }

    pub fn update(
        &mut self,
        layer: &mut RasterLayer,
        sample: BrushSample,
    ) -> Result<(), BrushError> {
        let brush = self.brush;
        let gesture = self.state.gesture;
        self.state.update(sample, |dab, direction| {
            brush.paint_dab(layer, gesture, dab, direction)
        })
    }

    pub fn finalize(&mut self, layer: &mut RasterLayer) -> Result<(), BrushError> {
        let brush = self.brush;
        let gesture = self.state.gesture;
        self.state
            .finalize(|dab, direction| brush.paint_dab(layer, gesture, dab, direction))
    }

    pub fn finish(mut self, layer: &mut RasterLayer) -> Result<Option<Damage>, BrushError> {
        self.finalize(layer)?;
        Ok(layer.commit_gesture(self.state.gesture)?)
    }

    pub fn cancel(self, layer: &mut RasterLayer) -> Result<Option<Damage>, BrushError> {
        Ok(layer.cancel_gesture(self.state.gesture)?)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaletteKnifeBrush {
    color: [f32; 3],
    diameter: f32,
    opacity: f32,
    spacing: f32,
}

impl PaletteKnifeBrush {
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
            spacing: (diameter * KNIFE_SPACING_FRACTION).max(0.5),
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

    pub fn contact_half_extents(self, pressure: f32) -> [f32; 2] {
        let pressure = pressure.clamp(0.0, 1.0);
        [
            self.diameter * 0.5 * (0.72 + 0.28 * pressure.sqrt()),
            self.diameter * 0.5 * (0.08 + 0.14 * pressure),
        ]
    }

    fn paint_dab(
        self,
        layer: &mut RasterLayer,
        gesture: GestureId,
        sample: BrushSample,
        direction: [f32; 2],
        deposits: &[LaneDeposit; KNIFE_LANE_COUNT],
    ) -> Result<(), BrushError> {
        if !sample.is_finite() {
            return Err(BrushError::InvalidSample);
        }
        let pressure = sample.pressure.clamp(0.0, 1.0);
        if pressure == 0.0 || self.opacity == 0.0 {
            return Ok(());
        }
        let half_extents = self.contact_half_extents(pressure);
        let Some(footprint) = OrientedBoxFootprint::new(
            layer.width(),
            layer.height(),
            sample.position,
            direction,
            half_extents,
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
                    .expect("the palette-knife bounds intersect every enumerated tile");
                let kernel = LaneDabKernel {
                    footprint,
                    deposits,
                    contact_fraction: 1.0,
                    lane_offset: 0.0,
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

#[derive(Clone, Copy, Debug, Default)]
struct LaneDeposit {
    color: [f32; 3],
    alpha: f32,
}

impl LaneDeposit {
    fn knife(index: usize, color: [f32; 3], opacity: f32) -> Self {
        let variation = hash_unit(index as u32, 0, 0x40d3_6f17);
        let load = 0.78 + variation * 0.22;
        let strength = 0.42 + variation * 0.58;
        Self {
            color,
            alpha: opacity * strength * (0.12 + 0.78 * load),
        }
    }

    fn bristle(index: usize, color: [f32; 3], opacity: f32) -> Self {
        let variation = hash_unit(index as u32, 0, 0x73f4_a821);
        let load = 0.7 + variation * 0.3;
        let strength = 0.48 + variation * 0.52;
        Self {
            color,
            alpha: opacity * strength * (0.1 + 0.72 * load),
        }
    }
}

pub struct PaletteKnifeStroke {
    brush: PaletteKnifeBrush,
    deposits: [LaneDeposit; KNIFE_LANE_COUNT],
    state: OrientedStrokeState,
}

impl PaletteKnifeStroke {
    pub fn begin(
        layer: &mut RasterLayer,
        brush: PaletteKnifeBrush,
        sample: BrushSample,
    ) -> Result<Self, BrushError> {
        if !sample.is_finite() {
            return Err(BrushError::InvalidSample);
        }
        let gesture = layer.begin_brush_gesture(brush.diameter())?;
        let mut state = OrientedStrokeState::new(gesture, sample, brush.spacing());
        let direction = state.orientation.resolve(sample);
        let deposits =
            std::array::from_fn(|index| LaneDeposit::knife(index, brush.color(), brush.opacity()));
        if let Err(error) = brush.paint_dab(layer, gesture, sample, direction, &deposits) {
            let _ = layer.cancel_gesture(gesture);
            return Err(error);
        }
        Ok(Self {
            brush,
            deposits,
            state,
        })
    }

    pub const fn gesture_id(&self) -> GestureId {
        self.state.gesture
    }

    pub const fn dabs_emitted(&self) -> u64 {
        self.state.dabs_emitted
    }

    pub fn update(
        &mut self,
        layer: &mut RasterLayer,
        sample: BrushSample,
    ) -> Result<(), BrushError> {
        let brush = self.brush;
        let gesture = self.state.gesture;
        let deposits = &self.deposits;
        self.state.update(sample, |dab, direction| {
            brush.paint_dab(layer, gesture, dab, direction, deposits)
        })
    }

    pub fn finalize(&mut self, layer: &mut RasterLayer) -> Result<(), BrushError> {
        let brush = self.brush;
        let gesture = self.state.gesture;
        let deposits = &self.deposits;
        self.state
            .finalize(|dab, direction| brush.paint_dab(layer, gesture, dab, direction, deposits))
    }

    pub fn finish(mut self, layer: &mut RasterLayer) -> Result<Option<Damage>, BrushError> {
        self.finalize(layer)?;
        Ok(layer.commit_gesture(self.state.gesture)?)
    }

    pub fn cancel(self, layer: &mut RasterLayer) -> Result<Option<Damage>, BrushError> {
        Ok(layer.cancel_gesture(self.state.gesture)?)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BristleBrush {
    color: [f32; 3],
    diameter: f32,
    opacity: f32,
    spacing: f32,
}

impl BristleBrush {
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
            spacing: (diameter * BRISTLE_SPACING_FRACTION).max(0.4),
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

    pub fn contact_half_extents(self, pressure: f32) -> [f32; 2] {
        let pressure = pressure.clamp(0.0, 1.0);
        [
            self.diameter * 0.5 * (0.55 + 0.45 * pressure.sqrt()),
            self.diameter * 0.5 * (0.12 + 0.2 * pressure),
        ]
    }

    fn paint_dab(
        self,
        layer: &mut RasterLayer,
        gesture: GestureId,
        sample: BrushSample,
        direction: [f32; 2],
        deposits: &[LaneDeposit; BRISTLE_COUNT],
    ) -> Result<(), BrushError> {
        if !sample.is_finite() {
            return Err(BrushError::InvalidSample);
        }
        let pressure = sample.pressure.clamp(0.0, 1.0);
        if pressure == 0.0 || self.opacity == 0.0 {
            return Ok(());
        }
        let half_extents = self.contact_half_extents(pressure);
        let Some(footprint) = OrientedBoxFootprint::new(
            layer.width(),
            layer.height(),
            sample.position,
            direction,
            half_extents,
        ) else {
            return Ok(());
        };

        let wobble = ((sample.position[0] * 0.071 + sample.position[1] * 0.053).sin() * 0.22)
            / BRISTLE_COUNT as f32;
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
                    .expect("the bristle bounds intersect every enumerated tile");
                let kernel = LaneDabKernel {
                    footprint,
                    deposits,
                    contact_fraction: BRISTLE_CONTACT_FRACTION,
                    lane_offset: wobble,
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

pub struct BristleStroke {
    brush: BristleBrush,
    deposits: [LaneDeposit; BRISTLE_COUNT],
    state: OrientedStrokeState,
}

impl BristleStroke {
    pub fn begin(
        layer: &mut RasterLayer,
        brush: BristleBrush,
        sample: BrushSample,
    ) -> Result<Self, BrushError> {
        if !sample.is_finite() {
            return Err(BrushError::InvalidSample);
        }
        let gesture = layer.begin_brush_gesture(brush.diameter())?;
        let mut state = OrientedStrokeState::new(gesture, sample, brush.spacing());
        let direction = state.orientation.resolve(sample);
        let deposits = std::array::from_fn(|index| {
            LaneDeposit::bristle(index, brush.color(), brush.opacity())
        });
        if let Err(error) = brush.paint_dab(layer, gesture, sample, direction, &deposits) {
            let _ = layer.cancel_gesture(gesture);
            return Err(error);
        }
        Ok(Self {
            brush,
            deposits,
            state,
        })
    }

    pub const fn gesture_id(&self) -> GestureId {
        self.state.gesture
    }

    pub const fn dabs_emitted(&self) -> u64 {
        self.state.dabs_emitted
    }

    pub fn update(
        &mut self,
        layer: &mut RasterLayer,
        sample: BrushSample,
    ) -> Result<(), BrushError> {
        let brush = self.brush;
        let gesture = self.state.gesture;
        let deposits = &self.deposits;
        self.state.update(sample, |dab, direction| {
            brush.paint_dab(layer, gesture, dab, direction, deposits)
        })
    }

    pub fn finalize(&mut self, layer: &mut RasterLayer) -> Result<(), BrushError> {
        let brush = self.brush;
        let gesture = self.state.gesture;
        let deposits = &self.deposits;
        self.state
            .finalize(|dab, direction| brush.paint_dab(layer, gesture, dab, direction, deposits))
    }

    pub fn finish(mut self, layer: &mut RasterLayer) -> Result<Option<Damage>, BrushError> {
        self.finalize(layer)?;
        Ok(layer.commit_gesture(self.state.gesture)?)
    }

    pub fn cancel(self, layer: &mut RasterLayer) -> Result<Option<Damage>, BrushError> {
        Ok(layer.cancel_gesture(self.state.gesture)?)
    }
}

pub struct FlatStroke {
    brush: FlatBrush,
    state: OrientedStrokeState,
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
        let mut state = OrientedStrokeState::new(gesture, sample, brush.spacing());
        let direction = state.orientation.resolve(sample);
        if let Err(error) = brush.paint_dab(layer, gesture, sample, direction) {
            let _ = layer.cancel_gesture(gesture);
            return Err(error);
        }
        Ok(Self { brush, state })
    }

    pub const fn gesture_id(&self) -> GestureId {
        self.state.gesture
    }

    pub const fn dabs_emitted(&self) -> u64 {
        self.state.dabs_emitted
    }

    pub fn update(
        &mut self,
        layer: &mut RasterLayer,
        sample: BrushSample,
    ) -> Result<(), BrushError> {
        let brush = self.brush;
        let gesture = self.state.gesture;
        self.state.update(sample, |dab, direction| {
            brush.paint_dab(layer, gesture, dab, direction)
        })
    }

    pub fn finalize(&mut self, layer: &mut RasterLayer) -> Result<(), BrushError> {
        let brush = self.brush;
        let gesture = self.state.gesture;
        self.state
            .finalize(|dab, direction| brush.paint_dab(layer, gesture, dab, direction))
    }

    pub fn finish(mut self, layer: &mut RasterLayer) -> Result<Option<Damage>, BrushError> {
        self.finalize(layer)?;
        Ok(layer.commit_gesture(self.state.gesture)?)
    }

    pub fn cancel(self, layer: &mut RasterLayer) -> Result<Option<Damage>, BrushError> {
        Ok(layer.cancel_gesture(self.state.gesture)?)
    }
}

struct OrientedStrokeState {
    gesture: GestureId,
    resampler: DistanceResampler,
    orientation: OrientationTracker,
    dabs_emitted: u64,
    finalized: bool,
}

impl OrientedStrokeState {
    fn new(gesture: GestureId, sample: BrushSample, spacing: f32) -> Self {
        Self {
            gesture,
            resampler: DistanceResampler::new(sample, spacing),
            orientation: OrientationTracker::new(sample.position),
            dabs_emitted: 1,
            finalized: false,
        }
    }

    fn update(
        &mut self,
        sample: BrushSample,
        mut emit: impl FnMut(BrushSample, [f32; 2]) -> Result<(), BrushError>,
    ) -> Result<(), BrushError> {
        if self.finalized {
            return Err(BrushError::StrokeFinalized);
        }
        if !sample.is_finite() {
            return Err(BrushError::InvalidSample);
        }
        let orientation = &mut self.orientation;
        self.dabs_emitted += self.resampler.update(sample, |dab| {
            let direction = orientation.resolve(dab);
            emit(dab, direction)
        })?;
        Ok(())
    }

    fn finalize(
        &mut self,
        mut emit: impl FnMut(BrushSample, [f32; 2]) -> Result<(), BrushError>,
    ) -> Result<(), BrushError> {
        if self.finalized {
            return Ok(());
        }
        let orientation = &mut self.orientation;
        self.dabs_emitted += self.resampler.finalize(|dab| {
            let direction = orientation.resolve(dab);
            emit(dab, direction)
        })?;
        self.finalized = true;
        Ok(())
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
        if let Some(direction) = contact_direction_from_tilt(sample.tilt) {
            self.direction = direction;
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
        self.coverage_at_local(local)
    }

    fn coverage_at_local(self, local: [f32; 2]) -> f32 {
        let q = [
            local[0].abs() - self.half_extents[0],
            local[1].abs() - self.half_extents[1],
        ];

        // The signed-distance form below is only necessary in the one-pixel
        // antialiasing fringe. Avoid its square root for the solid interior and
        // for the large empty corners of an oriented box's axis-aligned bounds.
        if q[0] >= 0.5 || q[1] >= 0.5 {
            return 0.0;
        }
        if q[0] <= 0.0 && q[1] <= 0.0 {
            return (0.5 - q[0].max(q[1])).min(1.0);
        }

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

struct LaneDabKernel<'a, const LANE_COUNT: usize> {
    footprint: OrientedBoxFootprint,
    deposits: &'a [LaneDeposit; LANE_COUNT],
    contact_fraction: f32,
    lane_offset: f32,
    local_damage: RectU32,
    tile_origin: [u32; 2],
}

impl<const LANE_COUNT: usize> LaneDabKernel<'_, LANE_COUNT> {
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
                let local = self.footprint.local_coordinates(world_x, world_y);
                let coverage = self.footprint.coverage_at_local(local);
                if coverage == 0.0 {
                    continue;
                }
                let lane_position =
                    (local[0] / self.footprint.half_extents[0] * 0.5 + 0.5 + self.lane_offset)
                        .clamp(0.0, 0.999_999);
                let lane_coordinate = lane_position * LANE_COUNT as f32;
                let lane_index = lane_coordinate as usize;
                let distance_from_center = (lane_coordinate - lane_index as f32 - 0.5).abs() * 2.0;
                let strand_coverage = ((self.contact_fraction - distance_from_center)
                    * self.footprint.half_extents[0]
                    / LANE_COUNT as f32
                    + 0.5)
                    .clamp(0.0, 1.0);
                if strand_coverage == 0.0 {
                    continue;
                }
                let deposit = self.deposits[lane_index];
                let source_alpha = (coverage * strand_coverage * deposit.alpha).clamp(0.0, 1.0);
                if source_alpha <= f32::EPSILON {
                    continue;
                }
                let keep_destination = 1.0 - source_alpha;
                let pixel = &mut pixels[row_start + local_x as usize];
                pixel.r = deposit.color[0] * source_alpha + pixel.r * keep_destination;
                pixel.g = deposit.color[1] * source_alpha + pixel.g * keep_destination;
                pixel.b = deposit.color[2] * source_alpha + pixel.b * keep_destination;
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

#[derive(Clone, Copy, Debug)]
struct OrientedEllipseFootprint {
    position: [f32; 2],
    direction: [f32; 2],
    half_extents: [f32; 2],
    min_x: u32,
    min_y: u32,
    max_x: u32,
    max_y: u32,
}

impl OrientedEllipseFootprint {
    fn new(
        canvas_width: u32,
        canvas_height: u32,
        position: [f32; 2],
        direction: [f32; 2],
        half_extents: [f32; 2],
    ) -> Option<Self> {
        let fringe = 0.5;
        let bound_x = ((direction[0] * half_extents[0]).powi(2)
            + (direction[1] * half_extents[1]).powi(2))
        .sqrt()
            + fringe;
        let bound_y = ((direction[1] * half_extents[0]).powi(2)
            + (direction[0] * half_extents[1]).powi(2))
        .sqrt()
            + fringe;
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

    fn coverage(self, pixel_x: u32, pixel_y: u32) -> f32 {
        let dx = pixel_x as f32 + 0.5 - self.position[0];
        let dy = pixel_y as f32 + 0.5 - self.position[1];
        let local_x = dx * self.direction[0] + dy * self.direction[1];
        let local_y = -dx * self.direction[1] + dy * self.direction[0];
        let normalized = ((local_x / self.half_extents[0]).powi(2)
            + (local_y / self.half_extents[1]).powi(2))
        .sqrt();
        let signed_distance = (normalized - 1.0) * self.half_extents[0].min(self.half_extents[1]);
        (0.5 - signed_distance).clamp(0.0, 1.0)
    }
}

struct PencilDabKernel {
    brush: PencilBrush,
    footprint: OrientedEllipseFootprint,
    transfer: f32,
    local_damage: RectU32,
    tile_origin: [u32; 2],
}

impl PencilDabKernel {
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
                let grain = paper_tooth(world_x, world_y, self.brush.grain_seed);
                let source_alpha =
                    (self.transfer * coverage * (0.18 + 0.82 * grain)).clamp(0.0, 1.0);
                if source_alpha <= f32::EPSILON {
                    continue;
                }
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

fn paper_tooth(x: u32, y: u32, seed: u32) -> f32 {
    let fine = hash_unit(x, y, seed);
    let fiber = hash_unit(x / 3, y / 3, seed ^ 0x68bc_21eb);
    0.68 * fine + 0.32 * fiber
}

fn hash_unit(x: u32, y: u32, seed: u32) -> f32 {
    let mut value = x
        .wrapping_mul(0x9e37_79b9)
        .wrapping_add(y.wrapping_mul(0x85eb_ca6b))
        .wrapping_add(seed);
    value ^= value >> 16;
    value = value.wrapping_mul(0x7feb_352d);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846c_a68b);
    value ^= value >> 16;
    (value >> 8) as f32 * (1.0 / 16_777_215.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::{HardRoundBrush, HardRoundStroke};
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

        assert!(horizontal.height() > horizontal.width() * 2);
        assert!(vertical.width() > vertical.height() * 2);
        assert_eq!(horizontal.height(), vertical.width());
        assert_eq!(horizontal.width(), vertical.height());
    }

    #[test]
    fn wacom_tilt_axis_is_rotated_into_the_contact_axis() {
        assert_eq!(contact_direction_from_tilt([0.8, 0.0]), Some([0.0, -1.0]));
        assert_eq!(contact_direction_from_tilt([0.0, 0.8]), Some([1.0, -0.0]));
        assert_eq!(contact_direction_from_tilt([0.01, 0.01]), None);
    }

    #[test]
    fn oriented_box_fast_paths_match_the_signed_distance_reference() {
        let footprint =
            OrientedBoxFootprint::new(256, 256, [128.0, 128.0], [0.8, 0.6], [29.0, 7.0]).unwrap();
        for y in -200..=200 {
            for x in -200..=200 {
                let local = [x as f32 * 0.17, y as f32 * 0.17];
                let q = [
                    local[0].abs() - footprint.half_extents[0],
                    local[1].abs() - footprint.half_extents[1],
                ];
                let outside =
                    (q[0].max(0.0) * q[0].max(0.0) + q[1].max(0.0) * q[1].max(0.0)).sqrt();
                let inside = q[0].max(q[1]).min(0.0);
                let reference = (0.5 - (outside + inside)).clamp(0.0, 1.0);
                let optimized = footprint.coverage_at_local(local);
                assert_eq!(
                    optimized.to_bits(),
                    reference.to_bits(),
                    "coverage differs at {local:?}: optimized={optimized} reference={reference}",
                );
            }
        }
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

    #[test]
    fn pencil_tilt_changes_point_contact_into_side_contact() {
        let pencil = PencilBrush::new([0.05; 3], 40.0, 1.0).unwrap();
        let mut upright = layer();
        PencilStroke::begin(&mut upright, pencil, BrushSample::new([128.0, 128.0], 1.0))
            .unwrap()
            .finish(&mut upright)
            .unwrap();
        let upright = painted_bounds(&upright);

        let mut tilted = layer();
        PencilStroke::begin(
            &mut tilted,
            pencil,
            BrushSample::with_tilt([128.0, 128.0], 1.0, [0.0, 0.9]),
        )
        .unwrap()
        .finish(&mut tilted)
        .unwrap();
        let tilted = painted_bounds(&tilted);

        assert!(tilted.width() > upright.width() * 4);
        assert!(tilted.height() > upright.height());
    }

    #[test]
    fn pencil_grain_is_anchored_to_world_pixels() {
        let a = paper_tooth(121, 77, PENCIL_GRAIN_SEED);
        let b = paper_tooth(121, 77, PENCIL_GRAIN_SEED);
        let neighbor = paper_tooth(122, 77, PENCIL_GRAIN_SEED);

        assert_eq!(a.to_bits(), b.to_bits());
        assert_ne!(a.to_bits(), neighbor.to_bits());
        assert!((0.0..=1.0).contains(&a));
    }

    #[test]
    fn pencil_replay_is_independent_of_collinear_packet_batching() {
        let pencil = PencilBrush::new([0.05; 3], 28.0, 0.8).unwrap();
        let first = BrushSample::with_tilt([24.0, 96.0], 0.4, [-0.1, 0.7]);
        let last = BrushSample::with_tilt([224.0, 96.0], 0.9, [-0.1, 0.7]);

        let mut direct = layer();
        let mut direct_stroke = PencilStroke::begin(&mut direct, pencil, first).unwrap();
        direct_stroke.update(&mut direct, last).unwrap();
        direct_stroke.finish(&mut direct).unwrap();

        let mut chunked = layer();
        let mut chunked_stroke = PencilStroke::begin(&mut chunked, pencil, first).unwrap();
        for index in 1..=4 {
            let t = index as f32 * 0.25;
            chunked_stroke
                .update(
                    &mut chunked,
                    BrushSample::with_tilt(
                        [
                            first.position[0] + (last.position[0] - first.position[0]) * t,
                            first.position[1],
                        ],
                        first.pressure + (last.pressure - first.pressure) * t,
                        first.tilt,
                    ),
                )
                .unwrap();
        }
        chunked_stroke.finish(&mut chunked).unwrap();

        assert_layers_close(&direct, &chunked);
    }

    #[test]
    fn palette_knife_keeps_cross_blade_lane_variation() {
        let knife = PaletteKnifeBrush::new([0.1, 0.3, 0.8], 72.0, 1.0).unwrap();
        let mut layer = layer();
        PaletteKnifeStroke::begin(
            &mut layer,
            knife,
            BrushSample::with_tilt([128.0, 128.0], 1.0, [0.0, 0.9]),
        )
        .unwrap()
        .finish(&mut layer)
        .unwrap();

        let first = layer.pixel(102, 128).unwrap().a;
        let second = layer.pixel(120, 128).unwrap().a;
        let third = layer.pixel(142, 128).unwrap().a;
        assert!(first > 0.0 && second > 0.0 && third > 0.0);
        assert!(first.to_bits() != second.to_bits() || second.to_bits() != third.to_bits());
    }

    #[test]
    fn palette_knife_deposits_keep_the_selected_color() {
        let mut layer = layer();
        let red = HardRoundBrush::new([1.0, 0.0, 0.0], 64.0, 1.0, 0.15).unwrap();
        HardRoundStroke::begin(&mut layer, red, BrushSample::new([190.0, 128.0], 1.0))
            .unwrap()
            .finish(&mut layer)
            .unwrap();

        let knife = PaletteKnifeBrush::new([0.0, 0.0, 1.0], 48.0, 0.8).unwrap();
        let mut stroke = PaletteKnifeStroke::begin(
            &mut layer,
            knife,
            BrushSample::with_tilt([60.0, 128.0], 1.0, [0.0, 0.8]),
        )
        .unwrap();
        stroke
            .update(
                &mut layer,
                BrushSample::with_tilt([190.0, 128.0], 1.0, [0.0, 0.8]),
            )
            .unwrap();

        assert!(stroke
            .deposits
            .iter()
            .all(|deposit| deposit.color == [0.0, 0.0, 1.0]));
        stroke.cancel(&mut layer).unwrap();
    }

    #[test]
    fn palette_knife_state_is_inline_and_bounded() {
        assert_eq!(KNIFE_LANE_COUNT, 12);
        assert_eq!(
            std::mem::size_of::<[LaneDeposit; KNIFE_LANE_COUNT]>(),
            std::mem::size_of::<LaneDeposit>() * KNIFE_LANE_COUNT
        );
    }

    #[test]
    fn bristle_contact_contains_painted_strands_and_real_gaps() {
        let brush = BristleBrush::new([0.6, 0.2, 0.05], 96.0, 1.0).unwrap();
        let mut layer = layer();
        BristleStroke::begin(
            &mut layer,
            brush,
            BrushSample::with_tilt([128.0, 128.0], 1.0, [0.0, 0.9]),
        )
        .unwrap()
        .finish(&mut layer)
        .unwrap();

        let mut painted = 0;
        let mut gaps = 0;
        for x in 82..174 {
            if layer.pixel(x, 128).unwrap().a > 0.0 {
                painted += 1;
            } else {
                gaps += 1;
            }
        }
        assert!(painted > 24);
        assert!(gaps > 8);
    }

    #[test]
    fn bristle_deposits_keep_the_selected_color() {
        let mut layer = layer();
        let red = HardRoundBrush::new([1.0, 0.0, 0.0], 52.0, 1.0, 0.15).unwrap();
        HardRoundStroke::begin(&mut layer, red, BrushSample::new([190.0, 128.0], 1.0))
            .unwrap()
            .finish(&mut layer)
            .unwrap();

        let brush = BristleBrush::new([0.0, 0.0, 1.0], 64.0, 0.8).unwrap();
        let mut stroke = BristleStroke::begin(
            &mut layer,
            brush,
            BrushSample::with_tilt([60.0, 128.0], 1.0, [0.0, 0.8]),
        )
        .unwrap();
        stroke
            .update(
                &mut layer,
                BrushSample::with_tilt([190.0, 128.0], 1.0, [0.0, 0.8]),
            )
            .unwrap();

        assert!(stroke
            .deposits
            .iter()
            .all(|deposit| deposit.color == [0.0, 0.0, 1.0]));
        stroke.cancel(&mut layer).unwrap();
    }

    #[test]
    fn bristle_state_is_inline_and_bounded() {
        assert_eq!(BRISTLE_COUNT, 24);
        assert_eq!(
            std::mem::size_of::<[LaneDeposit; BRISTLE_COUNT]>(),
            std::mem::size_of::<LaneDeposit>() * BRISTLE_COUNT
        );
    }
}
