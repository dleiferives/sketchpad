//! Gesture math shared by the canvas and the pen-operated brush puck.
//! Distances are logical pixels, so sensitivity does not change with display scale.
use winit::event::TouchPhase;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrushAxis {
    Size,
    Opacity,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrushDrag {
    pub origin: [f32; 2],
    pub diameter: f32,
    pub opacity: f32,
    pub axis: Option<BrushAxis>,
}

impl BrushDrag {
    pub fn new(origin: [f32; 2], diameter: f32, opacity: f32, axis: Option<BrushAxis>) -> Self {
        Self {
            origin,
            diameter,
            opacity,
            axis,
        }
    }

    pub fn update(&mut self, position: [f32; 2]) -> Option<(BrushAxis, f32)> {
        let dx = position[0] - self.origin[0];
        let dy = self.origin[1] - position[1];
        if !dx.is_finite() || !dy.is_finite() {
            return None;
        }
        if self.axis.is_none() {
            if dx.abs().max(dy.abs()) < 6.0 {
                return None;
            }
            self.axis = Some(if dx.abs() >= dy.abs() {
                BrushAxis::Size
            } else {
                BrushAxis::Opacity
            });
        }
        let axis = self.axis?;
        let value = match axis {
            BrushAxis::Size => (self.diameter * 2.0_f32.powf(dx / 100.0))
                .clamp(super::MIN_BRUSH_DIAMETER, super::MAX_BRUSH_DIAMETER),
            BrushAxis::Opacity => (self.opacity + dy / 200.0).clamp(0.0, 1.0),
        };
        Some((axis, value))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TouchAction {
    None,
    Navigate {
        from: [f32; 2],
        to: [f32; 2],
        scale: f32,
    },
    Undo,
    Redo,
}

#[derive(Clone, Copy, Debug)]
struct Contact {
    id: u64,
    origin: [f32; 2],
    position: [f32; 2],
}

#[derive(Debug, Default)]
pub struct TouchNavigation {
    contacts: Vec<Contact>,
    started_ms: u64,
    peak_contacts: usize,
    moved: bool,
    blocked: bool,
}

impl TouchNavigation {
    pub fn is_active(&self) -> bool {
        !self.contacts.is_empty()
    }

    pub fn suppress(&mut self) {
        self.blocked = true;
    }

    pub fn cancel(&mut self) {
        *self = Self::default();
    }

    /// A pen stroke blocks touch until every finger in that sequence has lifted.
    pub fn event(
        &mut self,
        id: u64,
        phase: TouchPhase,
        position: [f32; 2],
        now_ms: u64,
        pen_busy: bool,
    ) -> TouchAction {
        let invalid_position = !position.iter().all(|v| v.is_finite());
        if phase == TouchPhase::Started {
            if self.contacts.is_empty() {
                self.started_ms = now_ms;
                self.peak_contacts = 0;
                self.moved = false;
                self.blocked = pen_busy;
            }
            if !self.contacts.iter().any(|contact| contact.id == id) {
                self.contacts.push(Contact {
                    id,
                    origin: position,
                    position,
                });
            }
            self.peak_contacts = self.peak_contacts.max(self.contacts.len());
            self.blocked |= pen_busy || invalid_position || self.peak_contacts > 3;
            return TouchAction::None;
        }
        self.blocked |= pen_busy || invalid_position;
        let Some(index) = self.contacts.iter().position(|contact| contact.id == id) else {
            return TouchAction::None;
        };
        let before = self.pair();
        let origin = self.contacts[index].origin;
        self.moved |= (position[0] - origin[0]).hypot(position[1] - origin[1]) > 8.0;
        self.contacts[index].position = position;
        if matches!(phase, TouchPhase::Ended | TouchPhase::Cancelled) {
            self.blocked |= phase == TouchPhase::Cancelled;
            self.contacts.remove(index);
            if self.contacts.is_empty() {
                let action = if !self.blocked
                    && !self.moved
                    && now_ms.saturating_sub(self.started_ms) <= 300
                {
                    match self.peak_contacts {
                        2 => TouchAction::Undo,
                        3 => TouchAction::Redo,
                        _ => TouchAction::None,
                    }
                } else {
                    TouchAction::None
                };
                self.cancel();
                return action;
            }
            return TouchAction::None;
        }
        if !self.blocked && self.moved && self.peak_contacts == 2 {
            if let (Some((from, previous_span)), Some((to, span))) = (before, self.pair()) {
                return TouchAction::Navigate {
                    from,
                    to,
                    scale: if previous_span > 8.0 {
                        (span / previous_span).clamp(0.5, 2.0)
                    } else {
                        1.0
                    },
                };
            }
        }
        TouchAction::None
    }

    fn pair(&self) -> Option<([f32; 2], f32)> {
        if self.contacts.len() != 2 {
            return None;
        }
        let a = self.contacts[0].position;
        let b = self.contacts[1].position;
        Some((
            [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5],
            (a[0] - b[0]).hypot(a[1] - b[1]),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn puck_ignores_jitter_then_locks_one_axis() {
        let mut drag = BrushDrag::new([0.0, 0.0], 8.0, 0.5, None);
        assert_eq!(drag.update([2.0, -3.0]), None);
        assert_eq!(drag.update([100.0, -2.0]), Some((BrushAxis::Size, 16.0)));
        assert_eq!(drag.update([100.0, -180.0]), Some((BrushAxis::Size, 16.0)));
    }
    #[test]
    fn opacity_is_relative_reversible_and_clamped() {
        let mut drag = BrushDrag::new([20.0, 100.0], 8.0, 0.5, Some(BrushAxis::Opacity));
        assert_eq!(drag.update([20.0, 50.0]), Some((BrushAxis::Opacity, 0.75)));
        assert_eq!(drag.update([20.0, 100.0]), Some((BrushAxis::Opacity, 0.5)));
        assert_eq!(drag.update([20.0, 1000.0]), Some((BrushAxis::Opacity, 0.0)));
        assert_eq!(drag.update([f32::NAN, 0.0]), None);
    }
    #[test]
    fn size_changes_are_independent_of_event_frequency() {
        let mut a = BrushDrag::new([0.0; 2], 12.0, 1.0, Some(BrushAxis::Size));
        let mut b = a;
        for x in 0..100 {
            a.update([x as f32, 0.0]);
        }
        assert_eq!(a.update([100.0, 0.0]), b.update([100.0, 0.0]));
    }
    fn touch(
        n: &mut TouchNavigation,
        id: u64,
        phase: TouchPhase,
        position: [f32; 2],
        time: u64,
    ) -> TouchAction {
        n.event(id, phase, position, time, false)
    }
    #[test]
    fn three_finger_tap_emits_only_redo_after_all_contacts_end() {
        let mut n = TouchNavigation::default();
        for id in 0..3 {
            touch(&mut n, id, TouchPhase::Started, [id as f32 * 30.0, 0.0], 0);
        }
        for id in 0..2 {
            assert_eq!(
                touch(&mut n, id, TouchPhase::Ended, [id as f32 * 30.0, 0.0], 100),
                TouchAction::None
            );
        }
        assert_eq!(
            touch(&mut n, 2, TouchPhase::Ended, [60.0, 0.0], 120),
            TouchAction::Redo
        );
    }
    #[test]
    fn pinch_never_becomes_undo_on_lift() {
        let mut n = TouchNavigation::default();
        touch(&mut n, 1, TouchPhase::Started, [0.0, 0.0], 0);
        touch(&mut n, 2, TouchPhase::Started, [100.0, 0.0], 10);
        assert_eq!(
            touch(&mut n, 2, TouchPhase::Moved, [120.0, 0.0], 40),
            TouchAction::Navigate {
                from: [50.0, 0.0],
                to: [60.0, 0.0],
                scale: 1.2
            }
        );
        assert_eq!(
            touch(&mut n, 1, TouchPhase::Ended, [0.0, 0.0], 50),
            TouchAction::None
        );
        assert_eq!(
            touch(&mut n, 2, TouchPhase::Ended, [120.0, 0.0], 60),
            TouchAction::None
        );
    }
    #[test]
    fn pen_contact_or_cancellation_suppresses_the_entire_touch_sequence() {
        for cancelled in [false, true] {
            let mut n = TouchNavigation::default();
            n.event(1, TouchPhase::Started, [0.0, 0.0], 0, !cancelled);
            touch(&mut n, 2, TouchPhase::Started, [30.0, 0.0], 0);
            touch(
                &mut n,
                1,
                if cancelled {
                    TouchPhase::Cancelled
                } else {
                    TouchPhase::Ended
                },
                [0.0, 0.0],
                50,
            );
            assert_eq!(
                touch(&mut n, 2, TouchPhase::Ended, [30.0, 0.0], 80),
                TouchAction::None
            );
        }
    }
    #[test]
    fn pen_start_blocks_existing_fingers_even_without_another_touch_move() {
        let mut n = TouchNavigation::default();
        touch(&mut n, 1, TouchPhase::Started, [0.0; 2], 0);
        touch(&mut n, 2, TouchPhase::Started, [30.0, 0.0], 0);
        n.suppress();
        touch(&mut n, 1, TouchPhase::Ended, [0.0; 2], 100);
        assert_eq!(
            touch(&mut n, 2, TouchPhase::Ended, [30.0, 0.0], 100),
            TouchAction::None
        );
        assert!(!n.is_active());
    }

    #[test]
    fn invalid_touch_coordinates_cannot_trigger_history() {
        let mut n = TouchNavigation::default();
        touch(&mut n, 1, TouchPhase::Started, [f32::NAN, 0.0], 0);
        touch(&mut n, 2, TouchPhase::Started, [30.0, 0.0], 0);
        touch(&mut n, 1, TouchPhase::Ended, [0.0; 2], 100);
        assert_eq!(
            touch(&mut n, 2, TouchPhase::Ended, [30.0, 0.0], 100),
            TouchAction::None
        );
    }

    #[test]
    fn two_finger_tap_undo_and_single_finger_never_paints() {
        let mut n = TouchNavigation::default();
        touch(&mut n, 1, TouchPhase::Started, [0.0; 2], 0);
        assert_eq!(
            touch(&mut n, 1, TouchPhase::Ended, [0.0; 2], 100),
            TouchAction::None
        );
        touch(&mut n, 1, TouchPhase::Started, [0.0; 2], 200);
        touch(&mut n, 2, TouchPhase::Started, [30.0, 0.0], 210);
        touch(&mut n, 1, TouchPhase::Ended, [0.0; 2], 250);
        assert_eq!(
            touch(&mut n, 2, TouchPhase::Ended, [30.0, 0.0], 260),
            TouchAction::Undo
        );
    }
}
