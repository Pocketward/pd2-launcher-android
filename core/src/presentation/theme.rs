//! The launcher's theme, expressed as a GPUI Kit theme configuration.
//!
//! Teal (the icon's) marks only the main action, the current step and a
//! selection; amber means a warning and red an error. The brand mark's gold
//! belongs to the icon alone and is no slot here. Every text colour reaches
//! 4.5:1 on the canvas and on a card (the tests check it).
//!
//! The kit's `apply_config` sets both the theme colors and the token copy that
//! components paint with, and keeps the palette across any later
//! `Theme::change` or theme-registry reload.

use std::rc::Rc;

use gpui_kit::{
    App, Hsla, SharedString,
    component::{Theme, ThemeConfig, ThemeConfigColors, ThemeMode},
    rgba,
};

// Colors as 0xRRGGBBAA.
const CANVAS: u32 = 0x0e1214ff;
/// Cards, popovers and the corner pills' quiet fill.
const SURFACE: u32 = 0x141a1dff;
/// A hovered or focused row.
const RAISED: u32 = 0x1c2428ff;
/// Hairlines between rows and groups.
const RULE: u32 = 0x263036ff;
/// The edge of a quiet control floating over content, and of form controls:
/// opaque, because the kit halves the alpha of disabled controls.
const EDGE: u32 = 0x33404aff;
const TEXT: u32 = 0xe3e7e4ff;
const MUTED: u32 = 0x8b979cff;
/// Steps not reached yet and disabled labels: dimmer than MUTED.
const FAINT: u32 = 0x788489ff;
const TEAL: u32 = 0x34b8a7ff;
const TEAL_HOVER: u32 = 0x4fc7b7ff;
const TEAL_PRESSED: u32 = 0x2a9586ff;
/// The current step's and a selection's wash.
const TEAL_WASH: u32 = 0x34b8a729;
const ON_TEAL: u32 = 0x05201cff;
/// Warnings: an amber-orange between the brand's gold and red.
const AMBER: u32 = 0xee8a3aff;
const RED: u32 = 0xe5644fff;
// The title bar's window buttons: a light wash under the pointer, a
// stronger one while pressed, and a deeper red for a pressed Close.
const CONTROL_HOVER: u32 = 0xe3e7e414;
const CONTROL_PRESSED: u32 = 0xe3e7e426;
const RED_PRESSED: u32 = 0xc04a37ff;

/// Colors read directly for roles no kit theme slot names: the current
/// step's teal wash, done marks and links, faint labels, the corner pills,
/// and the title bar's window buttons.
#[derive(Clone, Copy)]
pub(crate) struct Palette {
    pub(crate) active_fill: Hsla,
    /// Done marks and text links.
    pub(crate) teal: Hsla,
    pub(crate) faint: Hsla,
    /// The corner pills: their quiet fill, under the pointer, and edge.
    pub(crate) surface: Hsla,
    pub(crate) raised: Hsla,
    pub(crate) edge: Hsla,
    pub(crate) control_hover: Hsla,
    pub(crate) control_pressed: Hsla,
    pub(crate) close_hover: Hsla,
    pub(crate) close_pressed: Hsla,
}

pub(crate) fn palette() -> Palette {
    Palette {
        active_fill: rgba(TEAL_WASH).into(),
        teal: rgba(TEAL).into(),
        faint: rgba(FAINT).into(),
        surface: rgba(SURFACE).into(),
        raised: rgba(RAISED).into(),
        edge: rgba(EDGE).into(),
        control_hover: rgba(CONTROL_HOVER).into(),
        control_pressed: rgba(CONTROL_PRESSED).into(),
        close_hover: rgba(RED).into(),
        close_pressed: rgba(RED_PRESSED).into(),
    }
}

/// The CD key fields' own colors (they are drawn by hand, not by the kit):
/// the text, an empty field's placeholder, the field and its edge, the
/// selection, and the caret and focused edge.
pub(crate) struct FieldColors {
    pub(crate) text: Hsla,
    pub(crate) placeholder: Hsla,
    pub(crate) fill: Hsla,
    pub(crate) edge: Hsla,
    pub(crate) selection: Hsla,
    pub(crate) focus: Hsla,
}

pub(crate) fn field_colors() -> FieldColors {
    FieldColors {
        text: rgba(TEXT).into(),
        placeholder: rgba(FAINT).into(),
        fill: rgba(CANVAS).into(),
        edge: rgba(EDGE).into(),
        selection: rgba(0x34b8a744).into(),
        focus: rgba(TEAL).into(),
    }
}

fn hex(color: u32) -> Option<SharedString> {
    Some(format!("#{color:08x}").into())
}

/// The palette mapped onto the kit's theme slots.
fn launcher_theme() -> ThemeConfig {
    // Some fields of the color set are private, so it is filled in place.
    let mut colors = ThemeConfigColors::default();
    colors.background = hex(CANVAS);
    colors.foreground = hex(TEXT);
    colors.border = hex(RULE);
    colors.input = hex(EDGE);
    colors.muted = hex(SURFACE);
    colors.muted_foreground = hex(MUTED);
    colors.popover = hex(SURFACE);
    colors.popover_foreground = hex(TEXT);
    colors.ring = hex(TEAL);
    colors.progress_bar = hex(TEAL);

    colors.primary = hex(TEAL);
    colors.primary_hover = hex(TEAL_HOVER);
    colors.primary_active = hex(TEAL_PRESSED);
    colors.primary_foreground = hex(ON_TEAL);
    colors.button_primary = hex(TEAL);
    colors.button_primary_hover = hex(TEAL_HOVER);
    colors.button_primary_active = hex(TEAL_PRESSED);
    colors.button_primary_foreground = hex(ON_TEAL);

    colors.secondary = hex(SURFACE);
    colors.secondary_hover = hex(RAISED);
    colors.secondary_active = hex(RAISED);
    colors.secondary_foreground = hex(TEXT);
    colors.switch = hex(EDGE);
    colors.switch_thumb = hex(MUTED);

    colors.info = hex(TEAL);
    colors.warning = hex(AMBER);
    colors.danger = hex(RED);
    colors.success = hex(TEAL);

    colors.title_bar = hex(CANVAS);
    colors.title_bar_border = hex(RULE);
    colors.tab_bar = hex(CANVAS);
    colors.tab_bar_segmented = hex(SURFACE);
    colors.tab = hex(CANVAS);
    colors.tab_active = hex(TEAL_WASH);
    colors.tab_foreground = hex(MUTED);
    colors.tab_active_foreground = hex(TEXT);

    colors.sidebar = hex(CANVAS);
    colors.sidebar_border = hex(RULE);
    colors.sidebar_foreground = hex(TEXT);
    colors.group_box = hex(SURFACE);
    colors.group_box_foreground = hex(TEXT);
    colors.description_list_label = hex(SURFACE);
    colors.description_list_label_foreground = hex(MUTED);

    ThemeConfig {
        name: "PD2 Launcher".into(),
        mode: ThemeMode::Dark,
        radius: Some(8),
        radius_lg: Some(12),
        colors,
        ..Default::default()
    }
}

/// Switch GPUI Kit to the launcher's dark theme. `apply_config` stores it as
/// the dark theme, and `Theme::change` then applies it and rebuilds the Base
/// theme (scrollbars) and text defaults from it.
pub(crate) fn apply(cx: &mut App) {
    Theme::global_mut(cx).apply_config(&Rc::new(launcher_theme()));
    Theme::change(ThemeMode::Dark, None, cx);
}

#[cfg(test)]
mod tests {
    use gpui_kit::{TestAppContext, component::ActiveTheme as _};

    use super::*;

    #[gpui_kit::test]
    fn the_palette_reaches_colors_and_tokens(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            apply(cx);

            let theme = cx.theme();
            assert!(theme.is_dark());
            assert_eq!(theme.background, Hsla::from(rgba(CANVAS)));
            assert_eq!(theme.border, Hsla::from(rgba(RULE)));
            assert_eq!(theme.warning, Hsla::from(rgba(AMBER)), "warnings are amber");
            assert_eq!(theme.primary, Hsla::from(rgba(TEAL)), "the icon's teal");
            assert_eq!(theme.input.a, 1.0, "radio rings stay visible when disabled");
            assert_eq!(
                theme.tokens.switch.color.a, 1.0,
                "switch tracks stay visible when disabled"
            );
            assert_eq!(
                theme.tokens.primary.color,
                Hsla::from(rgba(TEAL)),
                "components paint with the token copy"
            );
        });
    }

    /// WCAG 2 relative luminance of an opaque 0xRRGGBBAA color.
    fn luminance(color: u32) -> f64 {
        let channel = |shift: u32| {
            let value = f64::from((color >> shift) & 0xff) / 255.0;
            if value <= 0.039_28 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(24) + 0.7152 * channel(16) + 0.0722 * channel(8)
    }

    fn contrast(foreground: u32, background: u32) -> f64 {
        let (a, b) = (luminance(foreground), luminance(background));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn every_text_colour_reaches_four_and_a_half_to_one() {
        // The canvas and a card; a hovered row (RAISED) carries text only.
        for background in [CANVAS, SURFACE] {
            for (name, text) in [
                ("text", TEXT),
                ("muted", MUTED),
                ("faint", FAINT),
                ("teal", TEAL),
                ("amber", AMBER),
                ("red", RED),
            ] {
                let ratio = contrast(text, background);
                assert!(ratio >= 4.5, "{name} on {background:08x} is {ratio:.2}:1");
            }
        }
        assert!(contrast(TEXT, RAISED) >= 4.5);
        // The main action's label on its teal.
        assert!(contrast(ON_TEAL, TEAL) >= 4.5);
        assert!(contrast(ON_TEAL, TEAL_HOVER) >= 4.5);
        assert!(contrast(ON_TEAL, TEAL_PRESSED) >= 4.5);
    }

    #[test]
    fn warnings_have_their_own_amber_and_teal_is_the_icons() {
        // The icon's ring (packaging/io.github.kapdon.pd2-launcher.svg).
        let icon = std::str::from_utf8(crate::app_menu::ICON).expect("the icon is SVG text");
        assert!(icon.to_ascii_lowercase().contains("#34b8a7"));
        assert_ne!(AMBER, TEAL);
        assert_ne!(AMBER >> 8, 0xd6aa58, "no longer the brand's gold");
    }
}
