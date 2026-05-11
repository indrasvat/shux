//! Conversion utilities between shux-vt types and crossterm types.
//!
//! Kept in a separate module to centralize the VT-to-render mapping.

use crossterm::style::Color as CtColor;
use shux_vt::Color as VtColor;

/// Convert a shux-vt Color to a crossterm Color.
///
/// - `VtColor::Default` -> `None` (terminal default)
/// - `VtColor::Indexed(n)` -> `Some(CtColor::AnsiValue(n))`
/// - `VtColor::Rgb(r, g, b)` -> `Some(CtColor::Rgb { r, g, b })`
pub fn vt_color_to_crossterm(color: VtColor) -> Option<CtColor> {
    match color {
        VtColor::Default => None,
        VtColor::Indexed(n) => Some(CtColor::AnsiValue(n)),
        VtColor::Rgb(r, g, b) => Some(CtColor::Rgb { r, g, b }),
    }
}

/// Inverse of `vt_color_to_crossterm`. Named ANSI colours fold into their
/// indexed equivalents; unknown / impossible colours fall back to default.
pub fn crossterm_to_vt(color: CtColor) -> VtColor {
    match color {
        CtColor::Reset => VtColor::Default,
        CtColor::Black => VtColor::Indexed(0),
        CtColor::DarkRed => VtColor::Indexed(1),
        CtColor::DarkGreen => VtColor::Indexed(2),
        CtColor::DarkYellow => VtColor::Indexed(3),
        CtColor::DarkBlue => VtColor::Indexed(4),
        CtColor::DarkMagenta => VtColor::Indexed(5),
        CtColor::DarkCyan => VtColor::Indexed(6),
        CtColor::Grey => VtColor::Indexed(7),
        CtColor::DarkGrey => VtColor::Indexed(8),
        CtColor::Red => VtColor::Indexed(9),
        CtColor::Green => VtColor::Indexed(10),
        CtColor::Yellow => VtColor::Indexed(11),
        CtColor::Blue => VtColor::Indexed(12),
        CtColor::Magenta => VtColor::Indexed(13),
        CtColor::Cyan => VtColor::Indexed(14),
        CtColor::White => VtColor::Indexed(15),
        CtColor::AnsiValue(n) => VtColor::Indexed(n),
        CtColor::Rgb { r, g, b } => VtColor::Rgb(r, g, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_maps_to_none() {
        assert_eq!(vt_color_to_crossterm(VtColor::Default), None);
    }

    #[test]
    fn test_indexed_maps_to_ansi_value() {
        assert_eq!(
            vt_color_to_crossterm(VtColor::Indexed(196)),
            Some(CtColor::AnsiValue(196))
        );
    }

    #[test]
    fn test_rgb_maps_to_rgb() {
        assert_eq!(
            vt_color_to_crossterm(VtColor::Rgb(255, 128, 0)),
            Some(CtColor::Rgb {
                r: 255,
                g: 128,
                b: 0
            })
        );
    }
}
