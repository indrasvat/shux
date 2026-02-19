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
