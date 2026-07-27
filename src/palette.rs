use std::{error::Error, fmt};

pub const MAX_RECENT_COLORS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecentColorError {
    NonFiniteChannel,
    ChannelOutOfRange,
}

impl fmt::Display for RecentColorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFiniteChannel => formatter.write_str("color channels must be finite"),
            Self::ChannelOutOfRange => {
                formatter.write_str("color channels must be between zero and one")
            }
        }
    }
}

impl Error for RecentColorError {}

#[derive(Clone, Debug, PartialEq)]
pub struct RecentColors {
    colors: Vec<[f32; 3]>,
    selected: usize,
}

impl RecentColors {
    pub fn new(initial: [f32; 3]) -> Result<Self, RecentColorError> {
        validate_color(initial)?;
        let mut colors = Vec::with_capacity(MAX_RECENT_COLORS);
        colors.push(initial);
        Ok(Self {
            colors,
            selected: 0,
        })
    }

    pub fn colors(&self) -> &[[f32; 3]] {
        &self.colors
    }

    pub fn current(&self) -> [f32; 3] {
        self.colors[self.selected]
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn select(&mut self, color: [f32; 3]) -> Result<(), RecentColorError> {
        validate_color(color)?;
        if let Some(existing) = self.colors.iter().position(|candidate| *candidate == color) {
            self.colors.remove(existing);
        } else if self.colors.len() == MAX_RECENT_COLORS {
            self.colors.pop();
        }
        self.colors.insert(0, color);
        self.selected = 0;
        Ok(())
    }

    pub fn select_older(&mut self) -> [f32; 3] {
        self.selected = (self.selected + 1) % self.colors.len();
        self.current()
    }

    pub fn select_newer(&mut self) -> [f32; 3] {
        self.selected = (self.selected + self.colors.len() - 1) % self.colors.len();
        self.current()
    }
}

fn validate_color(color: [f32; 3]) -> Result<(), RecentColorError> {
    if color.iter().any(|channel| !channel.is_finite()) {
        return Err(RecentColorError::NonFiniteChannel);
    }
    if color.iter().any(|channel| !(0.0..=1.0).contains(channel)) {
        return Err(RecentColorError::ChannelOutOfRange);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{RecentColorError, RecentColors, MAX_RECENT_COLORS};

    #[test]
    fn starts_with_the_initial_color_selected() {
        let initial = [0.1, 0.2, 0.3];
        let recent = RecentColors::new(initial).unwrap();

        assert_eq!(recent.colors(), &[initial]);
        assert_eq!(recent.current(), initial);
        assert_eq!(recent.selected_index(), 0);
    }

    #[test]
    fn selecting_an_existing_color_moves_it_to_the_front_once() {
        let first = [0.1, 0.2, 0.3];
        let second = [0.4, 0.5, 0.6];
        let third = [0.7, 0.8, 0.9];
        let mut recent = RecentColors::new(first).unwrap();
        recent.select(second).unwrap();
        recent.select(third).unwrap();
        recent.select(second).unwrap();

        assert_eq!(recent.colors(), &[second, third, first]);
        assert_eq!(recent.current(), second);
    }

    #[test]
    fn capacity_evicts_the_least_recent_color() {
        let mut recent = RecentColors::new([0.0, 0.0, 0.0]).unwrap();
        for index in 1..=MAX_RECENT_COLORS {
            let value = index as f32 / MAX_RECENT_COLORS as f32;
            recent.select([value, value, value]).unwrap();
        }

        assert_eq!(recent.colors().len(), MAX_RECENT_COLORS);
        assert!(!recent.colors().contains(&[0.0, 0.0, 0.0]));
        assert_eq!(recent.current(), [1.0, 1.0, 1.0]);
    }

    #[test]
    fn navigation_wraps_without_reordering_history() {
        let first = [0.1, 0.2, 0.3];
        let second = [0.4, 0.5, 0.6];
        let third = [0.7, 0.8, 0.9];
        let mut recent = RecentColors::new(first).unwrap();
        recent.select(second).unwrap();
        recent.select(third).unwrap();
        let order = recent.colors().to_vec();

        assert_eq!(recent.select_older(), second);
        assert_eq!(recent.select_older(), first);
        assert_eq!(recent.select_older(), third);
        assert_eq!(recent.select_newer(), first);
        assert_eq!(recent.colors(), order);
    }

    #[test]
    fn rejects_invalid_channels_without_changing_history() {
        let initial = [0.1, 0.2, 0.3];
        let mut recent = RecentColors::new(initial).unwrap();

        assert_eq!(
            recent.select([f32::NAN, 0.0, 0.0]),
            Err(RecentColorError::NonFiniteChannel)
        );
        assert_eq!(
            recent.select([1.1, 0.0, 0.0]),
            Err(RecentColorError::ChannelOutOfRange)
        );
        assert_eq!(recent.colors(), &[initial]);
    }
}
