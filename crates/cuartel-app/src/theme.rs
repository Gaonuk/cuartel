use gpui::rgb;

pub struct Theme {
    pub bg_primary: u32,
    pub bg_secondary: u32,
    pub bg_sidebar: u32,
    pub bg_terminal: u32,
    pub bg_hover: u32,
    pub bg_active: u32,
    pub border: u32,
    pub text_primary: u32,
    pub text_secondary: u32,
    pub text_muted: u32,
    pub accent: u32,
    pub success: u32,
    pub warning: u32,
    pub error: u32,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            bg_primary: 0x1e1e2e,
            bg_secondary: 0x181825,
            bg_sidebar: 0x11111b,
            bg_terminal: 0x1e1e2e,
            bg_hover: 0x313244,
            bg_active: 0x45475a,
            border: 0x313244,
            text_primary: 0xcdd6f4,
            text_secondary: 0xbac2de,
            text_muted: 0x6c7086,
            accent: 0x89b4fa,
            success: 0xa6e3a1,
            warning: 0xf9e2af,
            error: 0xf38ba8,
        }
    }
}

pub fn rgb_color(hex: u32) -> gpui::Hsla {
    rgb(hex).into()
}
