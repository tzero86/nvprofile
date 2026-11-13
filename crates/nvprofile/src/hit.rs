//! Pure hit-testing and click logic for the TUI, kept free of terminal state so
//! it can be unit-tested. Rects come from the last drawn frame, never from
//! hard-coded coordinates.

use std::time::{Duration, Instant};

use crossterm::event::KeyCode;
use ratatui::layout::Rect;
use ratatui::text::Span;

/// Two clicks on the same row within this window count as a double-click.
pub const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// Width of an edit row's label column (`" power cap "`).
pub const EDIT_LABEL_W: u16 = 11;

/// A clickable footer button. Each one is equivalent to a key press, so mouse
/// and keyboard share exactly the same handlers (and confirm prompts).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Apply,
    Reset,
    Edit,
    Delete,
    Quit,
    Yes,
    No,
    Save,
    Confirm,
    Cancel,
}

impl Button {
    pub fn key(self) -> KeyCode {
        match self {
            Button::Apply | Button::Confirm => KeyCode::Enter,
            Button::Reset => KeyCode::Char('r'),
            Button::Edit => KeyCode::Char('e'),
            Button::Delete => KeyCode::Char('d'),
            Button::Quit => KeyCode::Char('q'),
            Button::Yes => KeyCode::Char('y'),
            Button::No => KeyCode::Char('n'),
            Button::Save => KeyCode::Char('s'),
            Button::Cancel => KeyCode::Esc,
        }
    }
}

/// A piece of the help line: plain text or a button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seg {
    pub text: String,
    pub button: Option<Button>,
}

impl Seg {
    pub fn text(s: &str) -> Self {
        Self { text: s.to_owned(), button: None }
    }
    pub fn btn(label: &str, b: Button) -> Self {
        Self { text: format!(" {label} "), button: Some(b) }
    }
    pub fn width(&self) -> u16 {
        Span::raw(self.text.as_str()).width() as u16
    }
}

pub fn total_width(segs: &[Seg]) -> u16 {
    segs.iter().map(Seg::width).sum()
}

/// The first variant that fits `width` columns, else the last (narrowest).
pub fn pick(mut variants: Vec<Vec<Seg>>, width: u16) -> Vec<Seg> {
    let idx = variants.iter().position(|v| total_width(v) <= width).unwrap_or(variants.len() - 1);
    variants.swap_remove(idx)
}

/// Screen rect of every button when `segs` is drawn left-aligned in `area`'s first row.
pub fn place(segs: &[Seg], area: Rect) -> Vec<(Button, Rect)> {
    let mut x = area.x;
    let mut out = Vec::new();
    for s in segs {
        let w = s.width();
        if let Some(b) = s.button {
            let right = area.right();
            if x < right {
                out.push((b, Rect::new(x, area.y, w.min(right - x), 1)));
            }
        }
        x = x.saturating_add(w);
    }
    out
}

pub fn contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.right() && y >= r.y && y < r.bottom()
}

pub fn button_at(buttons: &[(Button, Rect)], x: u16, y: u16) -> Option<Button> {
    buttons.iter().find(|(_, r)| contains(*r, x, y)).map(|(b, _)| *b)
}

/// Index of the list item under `(x, y)`. `inner` is the list's content area,
/// `offset` the index of the first visible item, one item per row.
pub fn list_row_at(inner: Rect, offset: usize, count: usize, x: u16, y: u16) -> Option<usize> {
    if !contains(inner, x, y) {
        return None;
    }
    let idx = offset + usize::from(y - inner.y);
    (idx < count).then_some(idx)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    Single,
    Double,
}

/// Detects double-clicks; time is injected so it is testable.
#[derive(Default)]
pub struct ClickTracker {
    last: Option<(usize, Instant)>,
}

impl ClickTracker {
    /// Register a click on row `idx`. A second click on the same row within
    /// [`DOUBLE_CLICK`] is a [`Click::Double`] and clears the history, so a
    /// third click starts over.
    pub fn click(&mut self, idx: usize, now: Instant) -> Click {
        match self.last {
            Some((i, t)) if i == idx && now.saturating_duration_since(t) <= DOUBLE_CLICK => {
                self.last = None;
                Click::Double
            }
            _ => {
                self.last = Some((idx, now));
                Click::Single
            }
        }
    }

    pub fn reset(&mut self) {
        self.last = None;
    }
}

/// Hit areas of one edit-mode row `" label     ◀ value ▶"` drawn at row
/// `y` of `inner`: (whole row, left arrow `" ◀ "`, right arrow `" ▶"`).
pub fn edit_row_rects(inner: Rect, y: u16, value_w: u16) -> (Rect, Rect, Rect) {
    let row = Rect::new(inner.x, y, inner.width, 1);
    let left = Rect::new(inner.x + EDIT_LABEL_W, y, 3, 1).intersection(row);
    let right = Rect::new(inner.x + EDIT_LABEL_W + 3 + value_w, y, 2, 1).intersection(row);
    (row, left, right)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segs() -> Vec<Seg> {
        vec![Seg::text("ab "), Seg::btn("Apply", Button::Apply), Seg::text(" "), Seg::btn("Quit", Button::Quit)]
    }

    #[test]
    fn place_matches_drawn_columns() {
        // "ab " (3) + " Apply " (7) + " " (1) + " Quit " (6)
        let p = place(&segs(), Rect::new(10, 20, 80, 1));
        assert_eq!(p, vec![(Button::Apply, Rect::new(13, 20, 7, 1)), (Button::Quit, Rect::new(21, 20, 6, 1))]);
        assert_eq!(button_at(&p, 13, 20), Some(Button::Apply));
        assert_eq!(button_at(&p, 19, 20), Some(Button::Apply));
        assert_eq!(button_at(&p, 20, 20), None, "the gap is not a button");
        assert_eq!(button_at(&p, 21, 20), Some(Button::Quit));
        assert_eq!(button_at(&p, 26, 20), Some(Button::Quit));
        assert_eq!(button_at(&p, 27, 20), None);
        assert_eq!(button_at(&p, 15, 21), None, "wrong row");
    }

    #[test]
    fn place_clips_buttons_to_the_area() {
        let p = place(&segs(), Rect::new(0, 0, 24, 1));
        assert_eq!(p[1], (Button::Quit, Rect::new(11, 0, 6, 1)));
        let narrow = place(&segs(), Rect::new(0, 0, 12, 1));
        assert_eq!(narrow.len(), 2);
        assert_eq!(narrow[1].1, Rect::new(11, 0, 1, 1));
        assert!(place(&segs(), Rect::new(0, 0, 11, 1)).len() == 1);
    }

    #[test]
    fn pick_chooses_the_widest_that_fits() {
        let long = vec![Seg::text("0123456789")];
        let short = vec![Seg::text("0123")];
        assert_eq!(pick(vec![long.clone(), short.clone()], 10), long);
        assert_eq!(pick(vec![long.clone(), short.clone()], 9), short);
        assert_eq!(pick(vec![long, short.clone()], 1), short, "falls back to the narrowest");
    }

    #[test]
    fn list_rows_respect_offset_bounds_and_count() {
        let inner = Rect::new(1, 4, 30, 5);
        assert_eq!(list_row_at(inner, 0, 4, 1, 4), Some(0));
        assert_eq!(list_row_at(inner, 0, 4, 30, 7), Some(3));
        assert_eq!(list_row_at(inner, 0, 4, 5, 8), None, "empty row below the last item");
        assert_eq!(list_row_at(inner, 2, 10, 5, 4), Some(2), "scrolled list");
        assert_eq!(list_row_at(inner, 0, 4, 0, 4), None, "border column");
        assert_eq!(list_row_at(inner, 0, 4, 31, 4), None, "right of the list");
        assert_eq!(list_row_at(inner, 0, 4, 5, 3), None, "above");
        assert_eq!(list_row_at(inner, 0, 4, 5, 9), None, "below");
    }

    #[test]
    fn double_click_needs_same_row_within_the_window() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut c = ClickTracker::default();
        assert_eq!(c.click(2, ms(0)), Click::Single);
        assert_eq!(c.click(2, ms(300)), Click::Double);
        assert_eq!(c.click(2, ms(350)), Click::Single, "history cleared after a double-click");

        let mut c = ClickTracker::default();
        assert_eq!(c.click(2, ms(0)), Click::Single);
        assert_eq!(c.click(2, ms(401)), Click::Single, "too slow");
        assert_eq!(c.click(2, ms(500)), Click::Double, "the slow click restarted the window");

        let mut c = ClickTracker::default();
        assert_eq!(c.click(1, ms(0)), Click::Single);
        assert_eq!(c.click(2, ms(100)), Click::Single, "different row");
        assert_eq!(c.click(1, ms(200)), Click::Single);

        let mut c = ClickTracker::default();
        c.click(1, ms(0));
        c.reset();
        assert_eq!(c.click(1, ms(10)), Click::Single);
    }

    #[test]
    fn edit_row_arrows_follow_the_drawn_text() {
        // " power cap " (11) + " ◀ " + "130.0 W" (7) + " ▶"
        let inner = Rect::new(35, 22, 70, 5);
        let (row, left, right) = edit_row_rects(inner, 23, 7);
        assert_eq!(row, Rect::new(35, 23, 70, 1));
        assert_eq!(left, Rect::new(46, 23, 3, 1));
        assert_eq!(right, Rect::new(56, 23, 2, 1));
        assert!(contains(left, 47, 23) && !contains(left, 49, 23));
        assert!(contains(right, 57, 23) && !contains(right, 58, 23));
    }

    #[test]
    fn edit_row_arrows_are_clipped_to_a_narrow_panel() {
        let (_, left, right) = edit_row_rects(Rect::new(0, 0, 14, 3), 0, 20);
        assert_eq!(left, Rect::new(11, 0, 3, 1));
        assert!(right.is_empty());
    }

    #[test]
    fn buttons_map_to_the_same_keys_as_the_keyboard() {
        use KeyCode::*;
        let m = [
            (Button::Apply, Enter),
            (Button::Reset, Char('r')),
            (Button::Edit, Char('e')),
            (Button::Delete, Char('d')),
            (Button::Quit, Char('q')),
            (Button::Yes, Char('y')),
            (Button::No, Char('n')),
            (Button::Save, Char('s')),
            (Button::Confirm, Enter),
            (Button::Cancel, Esc),
        ];
        for (b, k) in m {
            assert_eq!(b.key(), k);
        }
    }
}
