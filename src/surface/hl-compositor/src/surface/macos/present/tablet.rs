use super::PresenterEvent;

#[derive(Default)]
pub(super) struct Tablet {
    near: bool,
    down: bool,
}

impl Tablet {
    pub(super) fn proximity(&mut self, entering: bool, x: f64, y: f64) -> Vec<PresenterEvent> {
        if entering {
            if self.near {
                return Vec::new();
            }
            self.near = true;
            return vec![PresenterEvent::TabletProximityIn { x, y }];
        }

        let mut events = Vec::with_capacity(2);
        if self.down {
            events.push(PresenterEvent::TabletTipUp);
        }
        if self.near {
            events.push(PresenterEvent::TabletProximityOut);
        }
        self.down = false;
        self.near = false;
        events
    }

    pub(super) fn point(&mut self, x: f64, y: f64, pressure: f64) -> Vec<PresenterEvent> {
        let mut events = Vec::with_capacity(3);
        if !self.near {
            self.near = true;
            events.push(PresenterEvent::TabletProximityIn { x, y });
        }
        events.push(PresenterEvent::TabletMotion { x, y, pressure });
        let down = pressure > 0.0;
        if down != self.down {
            events.push(if down {
                PresenterEvent::TabletTipDown
            } else {
                PresenterEvent::TabletTipUp
            });
            self.down = down;
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::{PresenterEvent, Tablet};

    #[test]
    fn tablet_lifecycle_is_ordered_and_tip_transitions_are_not_duplicated() {
        let mut tablet = Tablet::default();
        assert!(matches!(
            tablet.point(10.0, 20.0, 0.5).as_slice(),
            [
                PresenterEvent::TabletProximityIn { .. },
                PresenterEvent::TabletMotion { pressure: 0.5, .. },
                PresenterEvent::TabletTipDown
            ]
        ));
        assert!(matches!(
            tablet.point(11.0, 21.0, 0.7).as_slice(),
            [PresenterEvent::TabletMotion { pressure: 0.7, .. }]
        ));
        assert!(matches!(
            tablet.proximity(false, 0.0, 0.0).as_slice(),
            [
                PresenterEvent::TabletTipUp,
                PresenterEvent::TabletProximityOut
            ]
        ));
    }
}
