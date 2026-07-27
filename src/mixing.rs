use std::{error::Error, fmt};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MixingError {
    InvalidColor,
    InvalidStrength,
    InvalidPickup,
    InvalidColorRate,
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
        }
    }
}

impl Error for MixingError {}

fn unit_value(value: f32) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipe(pickup: f32, color_rate: f32) -> MixingRecipeV1 {
        MixingRecipeV1::new(LinearRgb::new(1.0, 0.0, 0.0), pickup, color_rate).unwrap()
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
}
