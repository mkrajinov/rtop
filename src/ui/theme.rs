//! Colours: only Tailwind CSS palette values. A theme (a Tailwind gray family
//! plus Tailwind status hues) and one Tailwind accent are both switchable at
//! runtime (`T`).
//! Everything is 24-bit; terminals without truecolor get the nearest
//! 256-colour match (see `finalize`).
//! Highlights are background fills or inline glyphs, never edge stripes.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier, Style};

use crate::model::ProcState;

const fn hex(v: u32) -> Color {
    Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// A complete colour scheme. The accent is chosen separately.
pub struct Theme {
    pub name: &'static str,
    /// bright, text, dim, faint, selection base, header background.
    ui: [u32; 6],
    /// good, warn, bad, zombie, root, code.
    status: [u32; 6],
}

impl Theme {
    /// The theme's grays (light to dark) and its good/warn/bad/zombie hues,
    /// shown side by side in the picker.
    pub fn swatch(&self) -> ([Color; 6], [Color; 4]) {
        (self.ui.map(hex), [self.status[0], self.status[1], self.status[2], self.status[3]].map(hex))
    }
}

/// Every colour below is an exact Tailwind CSS v4 palette value (shade noted
/// in the comments), converted from Tailwind's oklch definitions to sRGB.
/// A theme pairs a Tailwind gray family with Tailwind hues for status.
pub const THEMES: &[Theme] = &[
    Theme {
        name: "neutral",
        ui: [0xf5f5f5, 0xe5e5e5, 0x737373, 0x404040, 0x262626, 0x171717], // neutral 100 200 500 700 800 900
        status: [0x00d492, 0xffb900, 0xff637e, 0xa684ff, 0xfee685, 0x46ecd5], // emerald-400 amber-400 rose-400 violet-400 amber-200 teal-300
    },
    Theme {
        name: "zinc",
        ui: [0xf4f4f5, 0xe4e4e7, 0x71717b, 0x3f3f46, 0x27272a, 0x18181b], // zinc 100 200 500 700 800 900
        status: [0x00d492, 0xffb900, 0xff637e, 0xa684ff, 0xfee685, 0x46ecd5], // emerald-400 amber-400 rose-400 violet-400 amber-200 teal-300
    },
    Theme {
        name: "stone",
        ui: [0xf5f5f4, 0xe7e5e4, 0x79716b, 0x44403b, 0x292524, 0x1c1917], // stone 100 200 500 700 800 900
        status: [0x00d492, 0xffb900, 0xff637e, 0xa684ff, 0xfee685, 0x46ecd5], // emerald-400 amber-400 rose-400 violet-400 amber-200 teal-300
    },
    Theme {
        name: "slate",
        ui: [0xf1f5f9, 0xe2e8f0, 0x62748e, 0x314158, 0x1d293d, 0x0f172b], // slate 100 200 500 700 800 900
        status: [0x00d492, 0xffb900, 0xff637e, 0xa684ff, 0xfee685, 0x46ecd5], // emerald-400 amber-400 rose-400 violet-400 amber-200 teal-300
    },
    Theme {
        name: "gray",
        ui: [0xf3f4f6, 0xe5e7eb, 0x6a7282, 0x364153, 0x1e2939, 0x101828], // gray 100 200 500 700 800 900
        status: [0x00d492, 0xffb900, 0xff637e, 0xa684ff, 0xfee685, 0x46ecd5], // emerald-400 amber-400 rose-400 violet-400 amber-200 teal-300
    },
    Theme {
        name: "taupe",
        ui: [0xf3f1f1, 0xe8e4e3, 0x7c6d67, 0x473c39, 0x2b2422, 0x1d1816], // taupe 100 200 500 700 800 900
        status: [0x00d492, 0xffb900, 0xff637e, 0xa684ff, 0xfee685, 0x46ecd5], // emerald-400 amber-400 rose-400 violet-400 amber-200 teal-300
    },
    Theme {
        name: "mauve",
        ui: [0xf3f1f3, 0xe7e4e7, 0x79697b, 0x463947, 0x2a212c, 0x1d161e], // mauve 100 200 500 700 800 900
        status: [0x00d492, 0xffb900, 0xff637e, 0xa684ff, 0xfee685, 0x46ecd5], // emerald-400 amber-400 rose-400 violet-400 amber-200 teal-300
    },
    Theme {
        name: "olive",
        ui: [0xf4f4f0, 0xe8e8e3, 0x7c7c67, 0x474739, 0x2b2b22, 0x1d1d16], // olive 100 200 500 700 800 900
        status: [0x00d492, 0xffb900, 0xff637e, 0xa684ff, 0xfee685, 0x46ecd5], // emerald-400 amber-400 rose-400 violet-400 amber-200 teal-300
    },
    Theme {
        name: "mist",
        ui: [0xf1f3f3, 0xe3e7e8, 0x67787c, 0x394447, 0x22292b, 0x161b1d], // mist 100 200 500 700 800 900
        status: [0x00d492, 0xffb900, 0xff637e, 0xa684ff, 0xfee685, 0x46ecd5], // emerald-400 amber-400 rose-400 violet-400 amber-200 teal-300
    },
    Theme {
        name: "pastel",
        ui: [0xf4f4f5, 0xe4e4e7, 0x71717b, 0x3f3f46, 0x27272a, 0x18181b], // zinc 100 200 500 700 800 900
        status: [0x5ee9b5, 0xffd230, 0xffa1ad, 0xc4b4ff, 0xffd6a7, 0x74d4ff], // emerald-300 amber-300 rose-300 violet-300 orange-200 sky-300
    },
    Theme {
        name: "vivid",
        ui: [0xf5f5f5, 0xe5e5e5, 0x737373, 0x404040, 0x262626, 0x171717], // neutral 100 200 500 700 800 900
        status: [0x00c950, 0xfdc700, 0xfb2c36, 0xe12afb, 0xff8904, 0x00d3f2], // green-500 yellow-400 red-500 fuchsia-500 orange-400 cyan-400
    },
    Theme {
        name: "ocean",
        ui: [0xf1f5f9, 0xe2e8f0, 0x62748e, 0x314158, 0x1d293d, 0x0f172b], // slate 100 200 500 700 800 900
        status: [0x00d5be, 0xffdf20, 0xfb64b6, 0x7c86ff, 0xb8e6fe, 0x53eafd], // teal-400 yellow-300 pink-400 indigo-400 sky-200 cyan-300
    },
    Theme {
        name: "forest",
        ui: [0xf4f4f0, 0xe8e8e3, 0x7c7c67, 0x474739, 0x2b2b22, 0x1d1d16], // olive 100 200 500 700 800 900
        status: [0x9ae600, 0xffb900, 0xff6467, 0xc27aff, 0xfff085, 0x5ee9b5], // lime-400 amber-400 red-400 purple-400 yellow-200 emerald-300
    },
    Theme {
        name: "sunset",
        ui: [0xf5f5f4, 0xe7e5e4, 0x79716b, 0x44403b, 0x292524, 0x1c1917], // stone 100 200 500 700 800 900
        status: [0x05df72, 0xffb86a, 0xff2056, 0xed6aff, 0xfee685, 0xffd6a7], // green-400 orange-300 rose-500 fuchsia-400 amber-200 orange-200
    },
    Theme {
        name: "dusk",
        ui: [0xf3f1f3, 0xe7e4e7, 0x79697b, 0x463947, 0x2a212c, 0x1d161e], // mauve 100 200 500 700 800 900
        status: [0x5ee9b5, 0xffdf20, 0xfb64b6, 0xc4b4ff, 0xffccd3, 0xdab2ff], // emerald-300 yellow-300 pink-400 violet-300 rose-200 purple-300
    },
    Theme {
        name: "frost",
        ui: [0xf1f3f3, 0xe3e7e8, 0x67787c, 0x394447, 0x22292b, 0x161b1d], // mist 100 200 500 700 800 900
        status: [0x46ecd5, 0xffd230, 0xffa2a2, 0xa3b3ff, 0xb8e6fe, 0xa2f4fd], // teal-300 amber-300 red-300 indigo-300 sky-200 cyan-200
    },
];

/// A Tailwind hue in three shades: 400 for text, 800 for the focused
/// column / active tab, 950 for the selected row.
pub struct Accent {
    pub name: &'static str,
    fg: u32,
    strong: u32,
    tint: u32,
}

/// Every Tailwind hue, in Tailwind's order.
pub const ACCENTS: &[Accent] = &[
    Accent { name: "red", fg: 0xff6467, strong: 0x9f0712, tint: 0x460809 },
    Accent { name: "orange", fg: 0xff8904, strong: 0x9f2d00, tint: 0x441306 },
    Accent { name: "amber", fg: 0xffb900, strong: 0x973c00, tint: 0x461901 },
    Accent { name: "yellow", fg: 0xfdc700, strong: 0x894b00, tint: 0x432004 },
    Accent { name: "lime", fg: 0x9ae600, strong: 0x3c6300, tint: 0x192e03 },
    Accent { name: "green", fg: 0x05df72, strong: 0x016630, tint: 0x032e15 },
    Accent { name: "emerald", fg: 0x00d492, strong: 0x006045, tint: 0x002c22 },
    Accent { name: "teal", fg: 0x00d5be, strong: 0x005f5a, tint: 0x022f2e },
    Accent { name: "cyan", fg: 0x00d3f2, strong: 0x005f78, tint: 0x053345 },
    Accent { name: "sky", fg: 0x00bcff, strong: 0x00598a, tint: 0x052f4a },
    Accent { name: "blue", fg: 0x51a2ff, strong: 0x193cb8, tint: 0x162456 },
    Accent { name: "indigo", fg: 0x7c86ff, strong: 0x372aac, tint: 0x1e1a4d },
    Accent { name: "violet", fg: 0xa684ff, strong: 0x5d0ec0, tint: 0x2f0d68 },
    Accent { name: "purple", fg: 0xc27aff, strong: 0x6e11b0, tint: 0x3c0366 },
    Accent { name: "fuchsia", fg: 0xed6aff, strong: 0x8a0194, tint: 0x4b004f },
    Accent { name: "pink", fg: 0xfb64b6, strong: 0xa3004c, tint: 0x510424 },
    Accent { name: "rose", fg: 0xff637e, strong: 0xa50036, tint: 0x4d0218 },
];

static THEME: AtomicUsize = AtomicUsize::new(0);
static ACCENT: AtomicUsize = AtomicUsize::new(1); // orange

/// Current (theme, accent) indexes into `THEMES` / `ACCENTS`.
pub fn current() -> (usize, usize) {
    (THEME.load(Ordering::Relaxed), ACCENT.load(Ordering::Relaxed))
}

pub fn accent_color(i: usize) -> Color {
    hex(ACCENTS[i % ACCENTS.len()].fg)
}

pub fn names() -> (&'static str, &'static str) {
    let (t, a) = current();
    (THEMES[t].name, ACCENTS[a].name)
}

pub fn set(theme: usize, accent: usize) {
    THEME.store(theme % THEMES.len(), Ordering::Relaxed);
    ACCENT.store(accent % ACCENTS.len(), Ordering::Relaxed);
}

/// Select by name; unknown names leave that half unchanged.
pub fn set_by_name(theme: Option<&str>, accent: Option<&str>) {
    let (mut t, mut a) = current();
    if let Some(i) = theme.and_then(|n| THEMES.iter().position(|x| x.name == n)) {
        t = i;
    }
    if let Some(i) = accent.and_then(|n| ACCENTS.iter().position(|x| x.name == n)) {
        a = i;
    }
    set(t, a);
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub bright: Color,
    pub text: Color,
    pub dim: Color,
    pub faint: Color,
    pub focus_bg: Color,
    pub sel_bg: Color,
    pub key_bg: Color,
    pub header_bg: Color,
    pub accent: Color,
    pub good: Color,
    pub warn: Color,
    pub bad: Color,
    pub zombie: Color,
    pub root: Color,
    pub code: Color,
}

pub fn pal() -> Palette {
    let (t, a) = current();
    let [bright, text, dim, faint, sel, header] = THEMES[t].ui.map(hex);
    let [good, warn, bad, zombie, root, code] = THEMES[t].status.map(hex);
    let accent = &ACCENTS[a];
    Palette {
        bright,
        text,
        dim,
        faint,
        // Highlight backgrounds use darker shades of the accent hue, so a
        // change is visible everywhere, not just in accent-coloured text.
        focus_bg: hex(accent.strong),
        sel_bg: hex(accent.tint),
        key_bg: sel,
        header_bg: header,
        accent: hex(accent.fg),
        good,
        warn,
        bad,
        zombie,
        root,
        code,
    }
}

pub fn selected() -> Style {
    Style::default().bg(pal().sel_bg).add_modifier(Modifier::BOLD)
}

pub fn dim() -> Style {
    Style::default().fg(pal().dim)
}

pub fn faint() -> Style {
    Style::default().fg(pal().faint)
}

pub fn text() -> Style {
    Style::default().fg(pal().text)
}

pub fn bold() -> Style {
    Style::default().fg(pal().text).add_modifier(Modifier::BOLD)
}

pub fn title() -> Style {
    Style::default().fg(pal().accent).add_modifier(Modifier::BOLD)
}

pub fn state(s: ProcState) -> Style {
    let c = match s {
        ProcState::Running => pal().good,
        ProcState::DiskSleep => pal().bad,
        ProcState::Zombie => pal().zombie,
        ProcState::Stopped | ProcState::Traced => pal().warn,
        ProcState::Unknown | ProcState::Idle => pal().faint,
        _ => pal().dim,
    };
    Style::default().fg(c)
}

/// Status colour for a 0–100% value: good below 50%, warn below 80%, then
/// bad. Stepped rather than blended, so every colour is a Tailwind value.
pub fn level(pct: f32) -> Color {
    let p = pal();
    match pct {
        x if x < 50.0 => p.good,
        x if x < 80.0 => p.warn,
        _ => p.bad,
    }
}

fn truecolor() -> bool {
    static TC: OnceLock<bool> = OnceLock::new();
    *TC.get_or_init(|| {
        let ct = std::env::var("COLORTERM").unwrap_or_default();
        let prog = std::env::var("TERM_PROGRAM").unwrap_or_default();
        ct.contains("truecolor")
            || ct.contains("24bit")
            || ["iTerm.app", "WarpTerminal", "vscode", "ghostty", "WezTerm"].contains(&prog.as_str())
    })
}

/// Map 24-bit colours to the nearest xterm-256 entry when the terminal needs it.
pub fn finalize(buf: &mut Buffer) {
    if truecolor() {
        return;
    }
    for cell in buf.content.iter_mut() {
        cell.fg = to_256(cell.fg);
        cell.bg = to_256(cell.bg);
    }
}

fn to_256(c: Color) -> Color {
    let Color::Rgb(r, g, b) = c else { return c };
    const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let near = |v: u8| STEPS.iter().enumerate().min_by_key(|(_, s)| (**s as i32 - v as i32).abs()).unwrap().0;
    let (ri, gi, bi) = (near(r), near(g), near(b));
    let cube = (STEPS[ri], STEPS[gi], STEPS[bi]);
    let avg = (r as u32 + g as u32 + b as u32) / 3;
    let gi_idx = ((avg.saturating_sub(8)) / 10).min(23) as u8;
    let gray = 8 + gi_idx * 10;
    let dist = |x: (u8, u8, u8)| {
        let d = |a: u8, b: u8| (a as i32 - b as i32).pow(2);
        d(x.0, r) + d(x.1, g) + d(x.2, b)
    };
    if dist((gray, gray, gray)) < dist(cube) {
        Color::Indexed(232 + gi_idx)
    } else {
        Color::Indexed(16 + 36 * ri as u8 + 6 * gi as u8 + bi as u8)
    }
}

/// The `T` overlay, shared by the process view and the k8s picker: arrow
/// keys apply a theme live, Enter saves it, Esc restores the old one.
pub struct Chooser {
    /// Which list has focus: 0 theme, 1 accent.
    pub focus: usize,
    before: (usize, usize),
}

/// What a key did to the [`Chooser`].
pub enum Chosen {
    Open,
    /// Closed; the message says where the theme was saved, if it was.
    Closed(Option<String>),
}

impl Chooser {
    pub fn open() -> Self {
        Chooser { focus: 0, before: current() }
    }

    pub fn key(&mut self, key: crossterm::event::KeyEvent) -> Chosen {
        use crossterm::event::KeyCode;
        let (g, a) = current();
        let step = |i: usize, n: usize, up: bool| if up { (i + n - 1) % n } else { (i + 1) % n };
        match key.code {
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => self.focus ^= 1,
            KeyCode::Up | KeyCode::Down if self.focus == 0 => set(step(g, THEMES.len(), key.code == KeyCode::Up), a),
            KeyCode::Up | KeyCode::Down => set(g, step(a, ACCENTS.len(), key.code == KeyCode::Up)),
            KeyCode::Esc | KeyCode::Char('q') => {
                set(self.before.0, self.before.1);
                return Chosen::Closed(None);
            }
            KeyCode::Enter => {
                let (name, accent) = names();
                let cfg = crate::config::Config { theme: Some(name.into()), accent: Some(accent.into()) };
                return Chosen::Closed(Some(match crate::config::save(&cfg) {
                    Ok(p) => format!("theme {name} + {accent} saved to {}", p.display()),
                    Err(e) => format!("theme applied, but saving failed: {e}"),
                }));
            }
            _ => {}
        }
        Chosen::Open
    }
}
