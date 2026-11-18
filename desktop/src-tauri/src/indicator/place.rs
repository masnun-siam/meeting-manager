//! Where the pill goes (FR-9): its remembered top-left when that still lands on a screen, else
//! centred 12 points below the menu bar. Everything here is in logical points of the global
//! screen space, top-left origin, y down, as Tauri's logical positions are.

use serde::{Deserialize, Serialize};

/// The gap between the menu bar and the pill's first position.
pub const BELOW_MENU_BAR: f64 = 12.0;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    fn intersects(&self, other: &Rect) -> bool {
        self.x < other.x + other.width
            && other.x < self.x + self.width
            && self.y < other.y + other.height
            && other.y < self.y + self.height
    }

    /// An AppKit rectangle (bottom-left origin, y up, from the primary screen's bottom) in this
    /// module's space; `primary_height` is the height of the screen with the menu bar.
    pub fn from_cocoa(x: f64, y: f64, width: f64, height: f64, primary_height: f64) -> Rect {
        Rect {
            x,
            y: primary_height - (y + height),
            width,
            height,
        }
    }
}

/// The pill's top-left: `saved` when the pill there intersects one of `screens`, else centred on
/// `menu_screen` (the visible frame of the screen with the menu bar) 12 points below its top.
pub fn place(saved: Option<Point>, screens: &[Rect], menu_screen: Rect, size: Size) -> Point {
    let on_a_screen = |p: &Point| {
        let pill = Rect {
            x: p.x,
            y: p.y,
            width: size.width,
            height: size.height,
        };
        screens.iter().any(|screen| pill.intersects(screen))
    };
    match saved.filter(on_a_screen) {
        Some(point) => point,
        None => Point {
            x: menu_screen.x + (menu_screen.width - size.width) / 2.0,
            y: menu_screen.y + BELOW_MENU_BAR,
        },
    }
}

/// The `indicator_position` setting; absent or unreadable JSON is no position.
pub fn parse(value: Option<&str>) -> Option<Point> {
    serde_json::from_str(value?).ok()
}

/// The `indicator_position` setting's value for `point`.
pub fn format(point: Point) -> String {
    serde_json::json!({"x": point.x, "y": point.y}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PILL: Size = Size {
        width: 220.0,
        height: 40.0,
    };
    /// A 1512 × 982 built-in display whose visible frame starts below a 33 pt menu bar.
    const BUILT_IN: Rect = Rect {
        x: 0.0,
        y: 0.0,
        width: 1512.0,
        height: 982.0,
    };
    const MENU: Rect = Rect {
        x: 0.0,
        y: 33.0,
        width: 1512.0,
        height: 949.0,
    };
    /// An external display to the right of it.
    const EXTERNAL: Rect = Rect {
        x: 1512.0,
        y: -200.0,
        width: 2560.0,
        height: 1440.0,
    };
    const TOP_CENTRE: Point = Point { x: 646.0, y: 45.0 };

    #[test]
    fn with_no_saved_point_it_is_top_centre_below_the_menu_bar() {
        assert_eq!(place(None, &[BUILT_IN], MENU, PILL), TOP_CENTRE);
    }

    #[test]
    fn a_saved_point_on_a_screen_is_kept() {
        let on_external = Point {
            x: 2000.0,
            y: 100.0,
        };
        assert_eq!(
            place(Some(on_external), &[BUILT_IN, EXTERNAL], MENU, PILL),
            on_external
        );
        // Partly off the edge still counts: the user can drag it back.
        let half_off = Point { x: -100.0, y: 10.0 };
        assert_eq!(place(Some(half_off), &[BUILT_IN], MENU, PILL), half_off);
    }

    #[test]
    fn a_saved_point_on_a_screen_that_is_gone_is_top_centre() {
        let on_external = Point {
            x: 2000.0,
            y: 100.0,
        };
        assert_eq!(
            place(Some(on_external), &[BUILT_IN], MENU, PILL),
            TOP_CENTRE
        );
        // Touching the edge is not on it.
        let beside = Point { x: 1512.0, y: 10.0 };
        assert_eq!(place(Some(beside), &[BUILT_IN], MENU, PILL), TOP_CENTRE);
        assert_eq!(place(Some(beside), &[], MENU, PILL), TOP_CENTRE);
    }

    #[test]
    fn unreadable_json_is_treated_as_absent() {
        for bad in [
            None,
            Some(""),
            Some("nope"),
            Some("{\"x\":1}"),
            Some(r#"{"x":"a","y":1}"#),
        ] {
            assert_eq!(parse(bad), None, "{bad:?}");
            assert_eq!(place(parse(bad), &[BUILT_IN], MENU, PILL), TOP_CENTRE);
        }
    }

    #[test]
    fn a_position_round_trips_through_the_setting() {
        let point = Point { x: 12.5, y: -3.0 };
        assert_eq!(format(point), r#"{"x":12.5,"y":-3.0}"#);
        assert_eq!(parse(Some(&format(point))), Some(point));
    }

    #[test]
    fn cocoa_rectangles_are_flipped_against_the_primary_screen() {
        // The external display's AppKit frame: its bottom is 1440 - 200 - 982 = 258 pt below
        // the primary's bottom.
        assert_eq!(
            Rect::from_cocoa(1512.0, -258.0, 2560.0, 1440.0, 982.0),
            EXTERNAL
        );
        // The built-in's visible frame: everything but the 33 pt menu bar.
        assert_eq!(Rect::from_cocoa(0.0, 0.0, 1512.0, 949.0, 982.0), MENU);
    }
}
