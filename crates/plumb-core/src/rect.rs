//! The rectangle shared by the treemap layout and the window layout, in whatever unit the caller uses.

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    /// Half-open: the left and top edges are inside, the right and bottom edges are not.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_is_half_open() {
        let r = Rect { x: 1.0, y: 2.0, w: 3.0, h: 4.0 };
        assert!(r.contains(1.0, 2.0) && r.contains(3.9, 5.9));
        assert!(!r.contains(4.0, 3.0) && !r.contains(2.0, 6.0) && !r.contains(0.9, 3.0));
    }
}
