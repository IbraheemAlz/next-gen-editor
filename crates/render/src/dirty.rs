//! Dirty-region tracking for incremental Canvas2D repaints
//! (PHASE_3_RENDER_RTL.md §9.4).
//!
//! Accumulates invalidated regions into one bounding rectangle; the renderer
//! clips to the drained region so an edit or scroll only repaints what
//! changed. A bounding-rect tracker — not the tile grid §9.4 sketches — since
//! it pairs directly with a single Canvas2D clip path.

use kurbo::Rect;

/// Accumulates invalidated regions into one bounding rectangle.
#[derive(Debug, Default)]
pub struct DirtyTracker {
    region: Option<Rect>,
}

impl DirtyTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark `rect` as needing repaint, unioning it into the pending region.
    pub fn invalidate(&mut self, rect: Rect) {
        self.region = Some(match self.region {
            Some(current) => current.union(rect),
            None => rect,
        });
    }

    /// Whether any region is pending repaint.
    pub fn is_dirty(&self) -> bool {
        self.region.is_some()
    }

    /// Take the accumulated region and reset the tracker to clean.
    pub fn drain(&mut self) -> Option<Rect> {
        self.region.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_clean() {
        let mut d = DirtyTracker::new();
        assert!(!d.is_dirty());
        assert_eq!(d.drain(), None);
    }

    #[test]
    fn invalidate_then_drain() {
        let mut d = DirtyTracker::new();
        d.invalidate(Rect::new(10.0, 10.0, 20.0, 20.0));
        assert!(d.is_dirty());
        assert_eq!(d.drain(), Some(Rect::new(10.0, 10.0, 20.0, 20.0)));
        assert!(!d.is_dirty());
    }

    #[test]
    fn unions_disjoint_regions() {
        let mut d = DirtyTracker::new();
        d.invalidate(Rect::new(0.0, 0.0, 10.0, 10.0));
        d.invalidate(Rect::new(20.0, 20.0, 30.0, 30.0));
        /* Bounding box enclosing both. */
        assert_eq!(d.drain(), Some(Rect::new(0.0, 0.0, 30.0, 30.0)));
    }

    #[test]
    fn contained_rect_is_absorbed() {
        let mut d = DirtyTracker::new();
        d.invalidate(Rect::new(0.0, 0.0, 100.0, 100.0));
        d.invalidate(Rect::new(40.0, 40.0, 60.0, 60.0));
        /* The inner rect leaves the bounding region unchanged. */
        assert_eq!(d.drain(), Some(Rect::new(0.0, 0.0, 100.0, 100.0)));
    }
}
