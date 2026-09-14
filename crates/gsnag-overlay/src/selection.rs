use gsnag_core::Rect;

type Point = (i64, i64);
const SNAP: i64 = 6;
const HANDLE: i64 = 7;

#[derive(Clone, Copy)]
enum Drag {
    Draw { anchor: Point, ratio: f64 },
    Move { start: Point, original: Rect },
}

pub(crate) struct Selection {
    pub bounds: Rect,
    pub outputs: Vec<Rect>,
    pub rect: Option<Rect>,
    pub pointer: Option<Point>,
    drag: Option<Drag>,
}

impl Selection {
    pub fn new(bounds: Rect, outputs: Vec<Rect>) -> Self {
        Self {
            bounds,
            outputs,
            rect: None,
            pointer: None,
            drag: None,
        }
    }

    fn clamp(&self, (x, y): Point) -> Point {
        (
            x.clamp(self.bounds.left, self.bounds.right),
            y.clamp(self.bounds.top, self.bounds.bottom),
        )
    }

    fn snap(&self, (x, y): Point) -> Point {
        // Only snap to the finite edge of an output, not an invisible extension.
        let snap_x = self
            .outputs
            .iter()
            .filter(|r| y >= r.top - SNAP && y <= r.bottom + SNAP)
            .flat_map(|r| [r.left, r.right])
            .filter(|edge| (edge - x).abs() <= SNAP)
            .min_by_key(|edge| (edge - x).abs())
            .unwrap_or(x);
        let snap_y = self
            .outputs
            .iter()
            .filter(|r| x >= r.left - SNAP && x <= r.right + SNAP)
            .flat_map(|r| [r.top, r.bottom])
            .filter(|edge| (edge - y).abs() <= SNAP)
            .min_by_key(|edge| (edge - y).abs())
            .unwrap_or(y);
        self.clamp((snap_x, snap_y))
    }

    pub fn begin(&mut self, point: Point) {
        let point = self.clamp(point);
        self.pointer = Some(point);
        if let Some(rect) = self.rect {
            // Four corner handles resize about the opposite corner.
            for (corner, anchor) in [
                ((rect.left, rect.top), (rect.right, rect.bottom)),
                ((rect.right, rect.top), (rect.left, rect.bottom)),
                ((rect.left, rect.bottom), (rect.right, rect.top)),
                ((rect.right, rect.bottom), (rect.left, rect.top)),
            ] {
                if (point.0 - corner.0).abs() <= HANDLE && (point.1 - corner.1).abs() <= HANDLE {
                    self.drag = Some(Drag::Draw {
                        anchor,
                        ratio: rect.width() as f64 / rect.height() as f64,
                    });
                    return;
                }
            }
            if rect.contains(point.0, point.1) {
                self.drag = Some(Drag::Move {
                    start: point,
                    original: rect,
                });
                return;
            }
        }
        self.drag = Some(Drag::Draw {
            anchor: self.snap(point),
            ratio: 1.0,
        });
        self.rect = None;
    }

    pub fn motion(&mut self, point: Point, proportional: bool) {
        let point = self.clamp(point);
        self.pointer = Some(point);
        match self.drag {
            Some(Drag::Draw { anchor, ratio }) => {
                let mut end = self.snap(point);
                if proportional {
                    let dx = end.0 - anchor.0;
                    let dy = end.1 - anchor.1;
                    let sx = if dx < 0 { -1 } else { 1 };
                    let sy = if dy < 0 { -1 } else { 1 };
                    let max_w = if sx < 0 {
                        anchor.0 - self.bounds.left
                    } else {
                        self.bounds.right - anchor.0
                    };
                    let max_h = if sy < 0 {
                        anchor.1 - self.bounds.top
                    } else {
                        self.bounds.bottom - anchor.1
                    };
                    let width = (dx.abs() as f64)
                        .max(dy.abs() as f64 * ratio)
                        .min(max_w as f64)
                        .min(max_h as f64 * ratio);
                    end = (
                        anchor.0 + sx * width.round() as i64,
                        anchor.1 + sy * (width / ratio).round() as i64,
                    );
                }
                let rect = Rect {
                    left: anchor.0.min(end.0),
                    top: anchor.1.min(end.1),
                    right: anchor.0.max(end.0),
                    bottom: anchor.1.max(end.1),
                };
                self.rect = (rect.width() > 0 && rect.height() > 0).then_some(rect);
            }
            Some(Drag::Move { start, original }) => {
                let dx = point.0 - start.0;
                let dy = point.1 - start.1;
                let moved = translated(original, dx, dy, self.bounds);
                let near = self.snap((moved.left, moved.top));
                let far = self.snap((moved.right, moved.bottom));
                let dx = if near.0 != moved.left {
                    near.0 - moved.left
                } else {
                    far.0 - moved.right
                };
                let dy = if near.1 != moved.top {
                    near.1 - moved.top
                } else {
                    far.1 - moved.bottom
                };
                self.rect = Some(translated(moved, dx, dy, self.bounds));
            }
            None => {}
        }
    }

    pub fn end(&mut self, point: Point, proportional: bool) {
        self.motion(point, proportional);
        self.drag = None;
    }

    pub fn nudge(&mut self, dx: i64, dy: i64, resize: bool) {
        if self.drag.is_some() {
            return;
        }
        if let Some(rect) = self.rect {
            self.rect = Some(if resize {
                Rect {
                    right: (rect.right + dx).clamp(rect.left + 1, self.bounds.right),
                    bottom: (rect.bottom + dy).clamp(rect.top + 1, self.bounds.bottom),
                    ..rect
                }
            } else {
                translated(rect, dx, dy, self.bounds)
            });
        }
    }

    pub fn accepted(&self) -> Option<Rect> {
        self.rect.filter(|r| {
            self.outputs
                .iter()
                .any(|output| output.intersection(*r).is_some())
        })
    }
}

fn translated(rect: Rect, dx: i64, dy: i64, bounds: Rect) -> Rect {
    let dx = dx.clamp(bounds.left - rect.left, bounds.right - rect.right);
    let dy = dy.clamp(bounds.top - rect.top, bounds.bottom - rect.bottom);
    Rect {
        left: rect.left + dx,
        right: rect.right + dx,
        top: rect.top + dy,
        bottom: rect.bottom + dy,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> Selection {
        let bounds = Rect {
            left: -200,
            top: -100,
            right: 300,
            bottom: 200,
        };
        Selection::new(
            bounds,
            vec![Rect { right: 0, ..bounds }, Rect { left: 20, ..bounds }],
        )
    }

    #[test]
    fn reverse_drag_crosses_outputs_and_snaps() {
        let mut s = model();
        s.begin((297, 197));
        s.end((-197, -97), false);
        assert_eq!(s.accepted(), Some(s.bounds));
        s.begin((100, 100));
        s.end((1000, 1000), false);
        assert_eq!(s.rect, Some(s.bounds)); // Whole desktop cannot move out of bounds.
    }

    #[test]
    fn move_resize_and_keyboard_stay_inside_bounds() {
        let mut s = model();
        s.begin((-100, -50));
        s.end((100, 50), false);
        s.begin((0, 0));
        s.end((40, 20), false);
        assert_eq!(
            s.rect.unwrap(),
            Rect {
                left: -60,
                top: -30,
                right: 140,
                bottom: 70
            }
        );
        s.begin((140, 70));
        s.end((200, 100), true);
        assert_eq!(s.rect.unwrap().width(), s.rect.unwrap().height() * 2);
        s.nudge(1000, 1000, false);
        assert_eq!(s.rect.unwrap().right, s.bounds.right);
        assert_eq!(s.rect.unwrap().bottom, s.bounds.bottom);
        s.nudge(-1000, -1000, true);
        assert_eq!(s.rect.unwrap().width(), 1);
        assert_eq!(s.rect.unwrap().height(), 1);
    }

    #[test]
    fn square_clamps_proportionally_and_empty_or_gap_is_not_accepted() {
        let mut s = model();
        s.begin((100, 100));
        s.end((290, 180), true);
        assert_eq!(
            s.rect.unwrap(),
            Rect {
                left: 100,
                top: 100,
                right: 200,
                bottom: 200
            }
        );
        s.begin((-100, 0));
        s.end((-100, 0), false);
        assert_eq!(s.accepted(), None);
        s.begin((8, 20));
        s.end((12, 40), false);
        assert_eq!(s.accepted(), None);
    }
}
