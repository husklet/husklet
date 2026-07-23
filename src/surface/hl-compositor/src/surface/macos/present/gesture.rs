use super::{NSEventPhase, PresenterEvent};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Active {
    Swipe,
    Pinch,
}

#[derive(Default)]
pub(super) struct Gestures {
    active: Option<Active>,
    scale: f64,
}

impl Gestures {
    pub(super) fn swipe(&mut self, phase: NSEventPhase, dx: f64, dy: f64) -> Vec<PresenterEvent> {
        let mut events = self.begin(Active::Swipe);
        events.push(PresenterEvent::GestureSwipeUpdate { dx, dy });
        if phase.intersects(NSEventPhase::Ended | NSEventPhase::Cancelled) {
            events.extend(self.end(phase.contains(NSEventPhase::Cancelled)));
        }
        events
    }

    pub(super) fn pinch(
        &mut self,
        phase: NSEventPhase,
        dx: f64,
        dy: f64,
        magnification: f64,
        rotation: f64,
    ) -> Vec<PresenterEvent> {
        let mut events = self.begin(Active::Pinch);
        self.scale *= (1.0 + magnification).max(f64::EPSILON);
        events.push(PresenterEvent::GesturePinchUpdate {
            dx,
            dy,
            scale: self.scale,
            rotation,
        });
        if phase.intersects(NSEventPhase::Ended | NSEventPhase::Cancelled) {
            events.extend(self.end(phase.contains(NSEventPhase::Cancelled)));
        }
        events
    }

    pub(super) fn end(&mut self, cancelled: bool) -> Vec<PresenterEvent> {
        match self.active.take() {
            Some(Active::Swipe) => vec![PresenterEvent::GestureSwipeEnd { cancelled }],
            Some(Active::Pinch) => vec![PresenterEvent::GesturePinchEnd { cancelled }],
            None => Vec::new(),
        }
    }

    fn begin(&mut self, next: Active) -> Vec<PresenterEvent> {
        if self.active == Some(next) {
            return Vec::new();
        }
        let mut events = self.end(true);
        self.active = Some(next);
        match next {
            Active::Swipe => events.push(PresenterEvent::GestureSwipeBegin { fingers: 3 }),
            Active::Pinch => {
                self.scale = 1.0;
                events.push(PresenterEvent::GesturePinchBegin { fingers: 2 });
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::{Gestures, NSEventPhase, PresenterEvent};

    #[test]
    fn swipe_has_one_ordered_lifecycle() {
        let mut gestures = Gestures::default();
        let began = gestures.swipe(NSEventPhase::Began, 1.0, -2.0);
        let ended = gestures.swipe(NSEventPhase::Ended, 0.5, 0.0);

        assert!(matches!(
            began.as_slice(),
            [
                PresenterEvent::GestureSwipeBegin { fingers: 3 },
                PresenterEvent::GestureSwipeUpdate { dx: 1.0, dy: -2.0 }
            ]
        ));
        assert!(matches!(
            ended.as_slice(),
            [
                PresenterEvent::GestureSwipeUpdate { dx: 0.5, dy: 0.0 },
                PresenterEvent::GestureSwipeEnd { cancelled: false }
            ]
        ));
    }

    #[test]
    fn pinch_accumulates_scale_and_switching_gestures_cancels_the_old_one() {
        let mut gestures = Gestures::default();
        let first = gestures.pinch(NSEventPhase::Began, 0.0, 0.0, 0.25, 3.0);
        let second = gestures.pinch(NSEventPhase::Changed, 1.0, 2.0, -0.2, -1.0);
        let switched = gestures.swipe(NSEventPhase::Changed, 4.0, 5.0);

        assert!(matches!(
            first.as_slice(),
            [
                PresenterEvent::GesturePinchBegin { fingers: 2 },
                PresenterEvent::GesturePinchUpdate { scale, .. }
            ] if (*scale - 1.25).abs() < f64::EPSILON
        ));
        assert!(matches!(
            second.as_slice(),
            [PresenterEvent::GesturePinchUpdate { scale, .. }]
                if (*scale - 1.0).abs() < f64::EPSILON
        ));
        assert!(matches!(
            switched.as_slice(),
            [
                PresenterEvent::GesturePinchEnd { cancelled: true },
                PresenterEvent::GestureSwipeBegin { fingers: 3 },
                PresenterEvent::GestureSwipeUpdate { .. }
            ]
        ));
    }
}
