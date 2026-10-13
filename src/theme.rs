#[derive(Clone, Copy)]
pub struct Theme {
    pub bg: u32,
    pub sidebar: u32,
    pub panel: u32,
    pub panel_hover: u32,
    pub text: u32,
    pub muted: u32,
    pub button: u32,
    pub button_hover: u32,
    pub selected: u32,
    pub accent: u32,
    pub status: u32,
    pub status_text: u32,
    pub border: u32,
    pub ok: u32,
    pub warn: u32,
    pub error: u32,
}

impl Theme {
    pub const DARK: Self = Self {
        bg: 0x101621,
        sidebar: 0x0b1019,
        panel: 0x192231,
        panel_hover: 0x202c40,
        text: 0xe8edf5,
        muted: 0x94a3b8,
        button: 0x253148,
        button_hover: 0x344766,
        selected: 0x435d88,
        accent: 0x32694e,
        status: 0x1b2b40,
        status_text: 0xb8d8fa,
        border: 0x344155,
        ok: 0x8ae0b1,
        warn: 0xf0c484,
        error: 0xf0b2a5,
    };

    pub const LIGHT: Self = Self {
        bg: 0xf4f5f8,
        sidebar: 0xe8ebf0,
        panel: 0xffffff,
        panel_hover: 0xedf0f6,
        text: 0x1a2030,
        muted: 0x5b6679,
        button: 0xe1e5ec,
        button_hover: 0xd0d7e2,
        selected: 0xbcd0f2,
        accent: 0x2e7d57,
        status: 0xe1ecf8,
        status_text: 0x23507f,
        border: 0xd3d8e1,
        ok: 0x1f7a4d,
        warn: 0x946000,
        error: 0xb3402e,
    };
}
