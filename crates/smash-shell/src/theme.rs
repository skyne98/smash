use material_color_utilities::dynamiccolor::{DynamicScheme, DynamicSchemeBuilder, Variant};
use material_color_utilities::hct::Hct;
use ratatui::style::Color;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SmashTheme {
    pub primary: Color,
    pub on_primary: Color,
    pub primary_container: Color,
    pub on_primary_container: Color,
    pub secondary: Color,
    pub on_secondary: Color,
    pub secondary_container: Color,
    pub on_secondary_container: Color,
    pub tertiary: Color,
    pub on_tertiary: Color,
    pub tertiary_container: Color,
    pub on_tertiary_container: Color,
    pub error: Color,
    pub on_error: Color,
    pub error_container: Color,
    pub on_error_container: Color,
    pub background: Color,
    pub on_background: Color,
    pub surface: Color,
    pub on_surface: Color,
    pub surface_variant: Color,
    pub on_surface_variant: Color,
    pub outline: Color,
    pub outline_variant: Color,
    pub shadow: Color,
}

impl SmashTheme {
    pub fn from_seed(seed_argb: u32, is_dark: bool) -> Self {
        let hct = Hct::from_int(seed_argb);
        let scheme = DynamicSchemeBuilder::default()
            .source_color_hct(hct)
            .variant(Variant::TonalSpot)
            .is_dark(is_dark)
            .build();

        Self::from_dynamic_scheme(&scheme)
    }

    pub fn from_dynamic_scheme(scheme: &DynamicScheme) -> Self {
        Self {
            primary: argb_to_color(scheme.primary()),
            on_primary: argb_to_color(scheme.on_primary()),
            primary_container: argb_to_color(scheme.primary_container()),
            on_primary_container: argb_to_color(scheme.on_primary_container()),
            secondary: argb_to_color(scheme.secondary()),
            on_secondary: argb_to_color(scheme.on_secondary()),
            secondary_container: argb_to_color(scheme.secondary_container()),
            on_secondary_container: argb_to_color(scheme.on_secondary_container()),
            tertiary: argb_to_color(scheme.tertiary()),
            on_tertiary: argb_to_color(scheme.on_tertiary()),
            tertiary_container: argb_to_color(scheme.tertiary_container()),
            on_tertiary_container: argb_to_color(scheme.on_tertiary_container()),
            error: argb_to_color(scheme.error()),
            on_error: argb_to_color(scheme.on_error()),
            error_container: argb_to_color(scheme.error_container()),
            on_error_container: argb_to_color(scheme.on_error_container()),
            background: argb_to_color(scheme.background()),
            on_background: argb_to_color(scheme.on_background()),
            surface: argb_to_color(scheme.surface()),
            on_surface: argb_to_color(scheme.on_surface()),
            surface_variant: argb_to_color(scheme.surface_variant()),
            on_surface_variant: argb_to_color(scheme.on_surface_variant()),
            outline: argb_to_color(scheme.outline()),
            outline_variant: argb_to_color(scheme.outline_variant()),
            shadow: argb_to_color(scheme.shadow()),
        }
    }
}

fn argb_to_color(argb: u32) -> Color {
    let r = ((argb >> 16) & 0xFF) as u8;
    let g = ((argb >> 8) & 0xFF) as u8;
    let b = (argb & 0xFF) as u8;
    Color::Rgb(r, g, b)
}

pub mod presets {
    pub const VIOLET: u32 = 0xFF6750A4;
    pub const OCEAN: u32 = 0xFF0061A4;
    pub const FOREST: u32 = 0xFF006E1C;
    pub const FIRE: u32 = 0xFFB91D1D;
    pub const GOLD: u32 = 0xFF725C00;
}
