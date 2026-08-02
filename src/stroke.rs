use crate::raster::LinearRgba;
use std::{error::Error, fmt, mem};

pub const BRUSH_RECIPE_VERSION: u32 = 1;
pub const DEFAULT_MINIMUM_PRESSURE_FRACTION: f32 = 0.05;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaintOperation {
    SourceOver,
    DestinationOut,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StrokeMaterial {
    color: [f32; 3],
    opacity: f32,
    flow: f32,
    operation: PaintOperation,
}

impl StrokeMaterial {
    pub fn paint(color: [f32; 3], opacity: f32, flow: f32) -> Result<Self, StrokeError> {
        Self::new(color, opacity, flow, PaintOperation::SourceOver)
    }

    pub fn eraser(opacity: f32, flow: f32) -> Result<Self, StrokeError> {
        Self::new([0.0; 3], opacity, flow, PaintOperation::DestinationOut)
    }

    pub fn new(
        color: [f32; 3],
        opacity: f32,
        flow: f32,
        operation: PaintOperation,
    ) -> Result<Self, StrokeError> {
        if color
            .iter()
            .any(|channel| !channel.is_finite() || !(0.0..=1.0).contains(channel))
        {
            return Err(StrokeError::InvalidColor);
        }
        if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
            return Err(StrokeError::InvalidOpacity);
        }
        if !flow.is_finite() || !(0.0..=1.0).contains(&flow) {
            return Err(StrokeError::InvalidFlow);
        }
        Ok(Self {
            color,
            opacity,
            flow,
            operation,
        })
    }

    pub const fn color(self) -> [f32; 3] {
        self.color
    }

    pub const fn opacity(self) -> f32 {
        self.opacity
    }

    pub const fn flow(self) -> f32 {
        self.flow
    }

    pub const fn operation(self) -> PaintOperation {
        self.operation
    }

    pub fn accumulation(self) -> StrokeAccumulation {
        if self.flow == 0.0 || self.opacity == 0.0 {
            StrokeAccumulation::None
        } else if self.flow == 1.0 {
            StrokeAccumulation::CoverageUnion
        } else {
            StrokeAccumulation::OpticalDensity { flow: self.flow }
        }
    }

    pub fn effect_alpha(self, accumulated_mask: f32) -> f32 {
        let response = match self.accumulation() {
            StrokeAccumulation::None => 0.0,
            StrokeAccumulation::CoverageUnion => accumulated_mask.clamp(0.0, 1.0),
            StrokeAccumulation::OpticalDensity { .. } => resolve_optical_density(accumulated_mask),
        };
        self.opacity * response
    }

    pub fn apply(self, destination: LinearRgba, accumulated_mask: f32) -> LinearRgba {
        let alpha = self.effect_alpha(accumulated_mask);
        if alpha == 0.0 {
            return destination;
        }
        let keep_destination = 1.0 - alpha;
        match self.operation {
            PaintOperation::SourceOver => LinearRgba::premultiplied(
                self.color[0] * alpha + destination.r * keep_destination,
                self.color[1] * alpha + destination.g * keep_destination,
                self.color[2] * alpha + destination.b * keep_destination,
                alpha + destination.a * keep_destination,
            ),
            PaintOperation::DestinationOut => LinearRgba::premultiplied(
                destination.r * keep_destination,
                destination.g * keep_destination,
                destination.b * keep_destination,
                destination.a * keep_destination,
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StrokeAccumulation {
    None,
    CoverageUnion,
    OpticalDensity { flow: f32 },
}

impl StrokeAccumulation {
    pub fn add_coverage(self, accumulated: f32, coverage: f32) -> Result<f32, StrokeError> {
        if !coverage.is_finite() || !(0.0..=1.0).contains(&coverage) {
            return Err(StrokeError::InvalidCoverage);
        }
        if accumulated.is_nan() || accumulated < 0.0 {
            return Err(StrokeError::InvalidAccumulation);
        }
        Ok(match self {
            Self::None => 0.0,
            Self::CoverageUnion => accumulated.max(coverage),
            Self::OpticalDensity { flow } => {
                accumulated + optical_density_contribution(flow, coverage)?
            }
        })
    }

    pub fn resolve(self, accumulated: f32) -> Result<f32, StrokeError> {
        if accumulated.is_nan() || accumulated < 0.0 {
            return Err(StrokeError::InvalidAccumulation);
        }
        Ok(match self {
            Self::None => 0.0,
            Self::CoverageUnion => accumulated.clamp(0.0, 1.0),
            Self::OpticalDensity { .. } => resolve_optical_density(accumulated),
        })
    }
}

pub fn optical_density_contribution(flow: f32, coverage: f32) -> Result<f32, StrokeError> {
    if !flow.is_finite() || !(0.0..1.0).contains(&flow) {
        return Err(StrokeError::FlowRequiresUnion(flow));
    }
    if !coverage.is_finite() || !(0.0..=1.0).contains(&coverage) {
        return Err(StrokeError::InvalidCoverage);
    }
    Ok(-(-flow * coverage).ln_1p())
}

pub fn resolve_optical_density(density: f32) -> f32 {
    if density <= 0.0 {
        return 0.0;
    }
    if density.is_infinite() {
        return 1.0;
    }
    -(-density).exp_m1()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoundBrushRecipeV1 {
    material: StrokeMaterial,
    diameter: f32,
    minimum_pressure_fraction: f32,
}

impl RoundBrushRecipeV1 {
    pub fn new(material: StrokeMaterial, diameter: f32) -> Result<Self, StrokeError> {
        Self::with_minimum_pressure_fraction(material, diameter, DEFAULT_MINIMUM_PRESSURE_FRACTION)
    }

    pub fn with_minimum_pressure_fraction(
        material: StrokeMaterial,
        diameter: f32,
        minimum_pressure_fraction: f32,
    ) -> Result<Self, StrokeError> {
        if !diameter.is_finite() || diameter <= 0.0 {
            return Err(StrokeError::InvalidDiameter);
        }
        if !minimum_pressure_fraction.is_finite()
            || !(0.0..=1.0).contains(&minimum_pressure_fraction)
        {
            return Err(StrokeError::InvalidMinimumPressure);
        }
        Ok(Self {
            material,
            diameter,
            minimum_pressure_fraction,
        })
    }

    pub const fn version(self) -> u32 {
        BRUSH_RECIPE_VERSION
    }

    pub const fn material(self) -> StrokeMaterial {
        self.material
    }

    pub const fn diameter(self) -> f32 {
        self.diameter
    }

    pub const fn minimum_pressure_fraction(self) -> f32 {
        self.minimum_pressure_fraction
    }

    pub fn radius_for_pressure(self, pressure: f32) -> f32 {
        self.diameter * 0.5 * pressure.clamp(0.0, 1.0).max(self.minimum_pressure_fraction)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimedBrushSample {
    pub position: [f32; 2],
    pub pressure: f32,
    pub tilt: [f32; 2],
    pub elapsed_micros: u64,
}

impl TimedBrushSample {
    pub const fn new(
        position: [f32; 2],
        pressure: f32,
        tilt: [f32; 2],
        elapsed_micros: u64,
    ) -> Self {
        Self {
            position,
            pressure,
            tilt,
            elapsed_micros,
        }
    }

    pub fn is_valid(self) -> bool {
        self.position.iter().all(|value| value.is_finite())
            && self.pressure.is_finite()
            && (0.0..=1.0).contains(&self.pressure)
            && self
                .tilt
                .iter()
                .all(|value| value.is_finite() && (-1.0..=1.0).contains(value))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoundContact {
    pub center: [f32; 2],
    pub radius: f32,
    pub elapsed_micros: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RoundPathCommand {
    Begin(RoundContact),
    Sweep {
        from: RoundContact,
        to: RoundContact,
    },
    End {
        at: RoundContact,
        elapsed_micros: u64,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoundPathBatch {
    commands: Vec<RoundPathCommand>,
}

impl RoundPathBatch {
    pub fn commands(&self) -> &[RoundPathCommand] {
        &self.commands
    }

    pub fn into_commands(self) -> Vec<RoundPathCommand> {
        self.commands
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

pub struct ContinuousRoundPath {
    recipe: RoundBrushRecipeV1,
    active_contact: Option<RoundContact>,
    last_elapsed_micros: Option<u64>,
    pending: Vec<RoundPathCommand>,
    commands_emitted: u64,
    finalized: bool,
}

impl ContinuousRoundPath {
    pub fn begin(
        recipe: RoundBrushRecipeV1,
        sample: TimedBrushSample,
    ) -> Result<Self, StrokeError> {
        let mut path = Self {
            recipe,
            active_contact: None,
            last_elapsed_micros: None,
            pending: Vec::new(),
            commands_emitted: 0,
            finalized: false,
        };
        path.update(sample)?;
        Ok(path)
    }

    pub const fn recipe(&self) -> RoundBrushRecipeV1 {
        self.recipe
    }

    pub const fn commands_emitted(&self) -> u64 {
        self.commands_emitted
    }

    pub fn update(&mut self, sample: TimedBrushSample) -> Result<(), StrokeError> {
        if self.finalized {
            return Err(StrokeError::StrokeFinalized);
        }
        if !sample.is_valid() {
            return Err(StrokeError::InvalidSample);
        }
        if self
            .last_elapsed_micros
            .is_some_and(|previous| sample.elapsed_micros < previous)
        {
            return Err(StrokeError::TimestampMovedBackward);
        }
        self.last_elapsed_micros = Some(sample.elapsed_micros);

        let contact_enabled = sample.pressure > 0.0
            && !matches!(
                self.recipe.material().accumulation(),
                StrokeAccumulation::None
            );
        if !contact_enabled {
            self.end_contact(sample.elapsed_micros);
            return Ok(());
        }

        let contact = RoundContact {
            center: sample.position,
            radius: self.recipe.radius_for_pressure(sample.pressure),
            elapsed_micros: sample.elapsed_micros,
        };
        match self.active_contact.replace(contact) {
            None => self.push(RoundPathCommand::Begin(contact)),
            Some(previous)
                if previous.center == contact.center && previous.radius == contact.radius => {}
            Some(previous) => self.push(RoundPathCommand::Sweep {
                from: previous,
                to: contact,
            }),
        }
        Ok(())
    }

    pub fn finish(&mut self) -> Result<(), StrokeError> {
        if self.finalized {
            return Err(StrokeError::StrokeFinalized);
        }
        self.finalized = true;
        let elapsed_micros = self.last_elapsed_micros.unwrap_or(0);
        self.end_contact(elapsed_micros);
        Ok(())
    }

    pub fn take_batch(&mut self) -> RoundPathBatch {
        RoundPathBatch {
            commands: mem::take(&mut self.pending),
        }
    }

    fn end_contact(&mut self, elapsed_micros: u64) {
        if let Some(at) = self.active_contact.take() {
            self.push(RoundPathCommand::End { at, elapsed_micros });
        }
    }

    fn push(&mut self, command: RoundPathCommand) {
        self.pending.push(command);
        self.commands_emitted = self.commands_emitted.saturating_add(1);
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StrokeError {
    InvalidColor,
    InvalidOpacity,
    InvalidFlow,
    InvalidCoverage,
    InvalidAccumulation,
    FlowRequiresUnion(f32),
    InvalidDiameter,
    InvalidMinimumPressure,
    InvalidSample,
    TimestampMovedBackward,
    StrokeFinalized,
}

impl fmt::Display for StrokeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidColor => write!(formatter, "stroke color is invalid"),
            Self::InvalidOpacity => write!(formatter, "stroke opacity is invalid"),
            Self::InvalidFlow => write!(formatter, "stroke flow is invalid"),
            Self::InvalidCoverage => write!(formatter, "stroke coverage is invalid"),
            Self::InvalidAccumulation => write!(formatter, "stroke accumulation is invalid"),
            Self::FlowRequiresUnion(flow) => write!(
                formatter,
                "flow {flow} must use coverage union instead of optical density"
            ),
            Self::InvalidDiameter => write!(formatter, "round brush diameter is invalid"),
            Self::InvalidMinimumPressure => {
                write!(formatter, "round brush minimum pressure is invalid")
            }
            Self::InvalidSample => write!(formatter, "timed brush sample is invalid"),
            Self::TimestampMovedBackward => write!(formatter, "stroke timestamp moved backward"),
            Self::StrokeFinalized => write!(formatter, "stroke is already finalized"),
        }
    }
}

impl Error for StrokeError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(x: f32, pressure: f32, elapsed_micros: u64) -> TimedBrushSample {
        TimedBrushSample::new([x, 10.0], pressure, [0.25, -0.5], elapsed_micros)
    }

    fn recipe(flow: f32) -> RoundBrushRecipeV1 {
        RoundBrushRecipeV1::new(
            StrokeMaterial::paint([0.2, 0.4, 0.8], 0.75, flow).unwrap(),
            40.0,
        )
        .unwrap()
    }

    #[test]
    fn flow_uses_union_only_at_full_strength() {
        assert_eq!(
            StrokeMaterial::paint([0.0; 3], 1.0, 0.0)
                .unwrap()
                .accumulation(),
            StrokeAccumulation::None
        );
        assert_eq!(
            StrokeMaterial::paint([0.0; 3], 1.0, 1.0)
                .unwrap()
                .accumulation(),
            StrokeAccumulation::CoverageUnion
        );
        assert_eq!(
            StrokeMaterial::paint([0.0; 3], 1.0, 0.25)
                .unwrap()
                .accumulation(),
            StrokeAccumulation::OpticalDensity { flow: 0.25 }
        );
    }

    #[test]
    fn optical_density_matches_repeated_source_over_and_opacity_caps_it() {
        let material = StrokeMaterial::paint([0.2, 0.4, 0.8], 0.5, 0.25).unwrap();
        let accumulation = material.accumulation();
        let once = accumulation.add_coverage(0.0, 1.0).unwrap();
        assert!((accumulation.resolve(once).unwrap() - 0.25).abs() < 1.0e-6);
        let twice = accumulation.add_coverage(once, 1.0).unwrap();
        assert!((accumulation.resolve(twice).unwrap() - 0.4375).abs() < 1.0e-6);
        assert!((material.effect_alpha(twice) - 0.21875).abs() < 1.0e-6);
    }

    #[test]
    fn full_flow_unions_coverage_without_self_darkening() {
        let accumulation = StrokeAccumulation::CoverageUnion;
        let first = accumulation.add_coverage(0.0, 0.6).unwrap();
        let second = accumulation.add_coverage(first, 0.3).unwrap();
        assert_eq!(accumulation.resolve(second), Ok(0.6));
    }

    #[test]
    fn paint_and_erase_apply_the_same_bounded_effect() {
        let destination = LinearRgba::from_straight(0.8, 0.4, 0.2, 0.75);
        let paint = StrokeMaterial::paint([0.1, 0.2, 0.3], 0.5, 1.0).unwrap();
        assert_eq!(
            paint.apply(destination, 1.0),
            LinearRgba::premultiplied(
                0.1 * 0.5 + destination.r * 0.5,
                0.2 * 0.5 + destination.g * 0.5,
                0.3 * 0.5 + destination.b * 0.5,
                0.5 + destination.a * 0.5,
            )
        );

        let erase = StrokeMaterial::eraser(0.5, 1.0).unwrap();
        assert_eq!(
            erase.apply(destination, 1.0),
            LinearRgba::premultiplied(
                destination.r * 0.5,
                destination.g * 0.5,
                destination.b * 0.5,
                destination.a * 0.5,
            )
        );
    }

    #[test]
    fn round_path_batches_do_not_change_the_command_stream() {
        let samples = [
            sample(2.0, 0.5, 0),
            sample(8.0, 0.75, 1_000),
            sample(14.0, 1.0, 2_000),
            sample(14.0, 0.0, 3_000),
            sample(20.0, 0.5, 4_000),
        ];

        let mut whole = ContinuousRoundPath::begin(recipe(1.0), samples[0]).unwrap();
        for sample in &samples[1..] {
            whole.update(*sample).unwrap();
        }
        whole.finish().unwrap();
        let whole = whole.take_batch().into_commands();

        let mut incremental = ContinuousRoundPath::begin(recipe(1.0), samples[0]).unwrap();
        let mut commands = incremental.take_batch().into_commands();
        for sample in &samples[1..] {
            incremental.update(*sample).unwrap();
            commands.extend(incremental.take_batch().into_commands());
        }
        incremental.finish().unwrap();
        commands.extend(incremental.take_batch().into_commands());

        assert_eq!(commands, whole);
    }

    #[test]
    fn contact_breaks_and_timestamps_are_monotonic() {
        let mut path = ContinuousRoundPath::begin(recipe(1.0), sample(2.0, 1.0, 10)).unwrap();
        path.update(sample(4.0, 0.0, 20)).unwrap();
        path.update(sample(6.0, 1.0, 30)).unwrap();
        assert_eq!(
            path.update(sample(8.0, 1.0, 29)),
            Err(StrokeError::TimestampMovedBackward)
        );
        path.finish().unwrap();
        assert!(matches!(
            path.update(sample(9.0, 1.0, 40)),
            Err(StrokeError::StrokeFinalized)
        ));
        let commands = path.take_batch().into_commands();
        assert!(matches!(commands[0], RoundPathCommand::Begin(_)));
        assert!(matches!(commands[1], RoundPathCommand::End { .. }));
        assert!(matches!(commands[2], RoundPathCommand::Begin(_)));
        assert!(matches!(commands[3], RoundPathCommand::End { .. }));
    }

    #[test]
    fn recipe_preserves_the_current_pressure_floor() {
        let recipe = recipe(1.0);
        assert_eq!(recipe.version(), 1);
        assert_eq!(recipe.radius_for_pressure(0.0), 1.0);
        assert_eq!(recipe.radius_for_pressure(0.5), 10.0);
        assert_eq!(recipe.radius_for_pressure(1.0), 20.0);
    }

    #[test]
    fn invalid_values_are_rejected_before_a_stroke_begins() {
        assert!(matches!(
            StrokeMaterial::paint([f32::NAN, 0.0, 0.0], 1.0, 1.0),
            Err(StrokeError::InvalidColor)
        ));
        assert!(matches!(
            StrokeMaterial::eraser(1.1, 1.0),
            Err(StrokeError::InvalidOpacity)
        ));
        assert!(matches!(
            StrokeMaterial::eraser(1.0, -0.1),
            Err(StrokeError::InvalidFlow)
        ));
        assert!(matches!(
            RoundBrushRecipeV1::new(StrokeMaterial::eraser(1.0, 1.0).unwrap(), 0.0),
            Err(StrokeError::InvalidDiameter)
        ));
    }
}
