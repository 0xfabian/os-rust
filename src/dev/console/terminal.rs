use super::font::FONT_BITMAP_8X8;
use core::fmt::Write;
use limine::framebuffer::Framebuffer as LimineFramebuffer;

struct Framebuffer<'a> {
    addr: &'a mut [u32],
    width: usize,
    height: usize,
    pitch: usize,
}

impl<'a> Framebuffer<'a> {
    fn from(fb: &LimineFramebuffer) -> Self {
        Framebuffer {
            addr: unsafe {
                core::slice::from_raw_parts_mut(
                    fb.addr() as *mut u32,
                    (fb.pitch() / 4 * fb.height()) as usize,
                )
            },
            width: fb.width() as usize,
            height: fb.height() as usize,
            pitch: fb.pitch() as usize,
        }
    }

    fn put_pixel(&mut self, x: usize, y: usize, color: u32) {
        let index = y * (self.pitch / 4) + x;
        self.addr[index] = color;
    }

    fn draw_glyph(&mut self, x: usize, y: usize, glyph: u8, fg_color: u32, bg_color: u32) {
        if glyph >= 128 {
            return; // Unsupported by current font
        }

        for col in 0..8 {
            self.put_pixel(x + col, y, bg_color);
        }

        let glyph_bitmap = &FONT_BITMAP_8X8[(glyph as usize) * 8..(glyph as usize + 1) * 8];
        for (row, byte) in glyph_bitmap.iter().enumerate() {
            for col in 0..8 {
                let pixel_on = (byte >> col) & 1;
                let color = if pixel_on != 0 { fg_color } else { bg_color };
                self.put_pixel(x + col, y + row + 1, color);
            }
        }

        for col in 0..8 {
            self.put_pixel(x + col, y + 9, bg_color);
        }
    }
}

#[derive(PartialEq)]
struct AnsiArgs {
    count: usize,
    args: [usize; 8], // support up to 8 args for simplicity
}

impl Default for AnsiArgs {
    fn default() -> Self {
        AnsiArgs {
            count: 1,
            args: [0; 8],
        }
    }
}

#[derive(PartialEq)]
enum AnsiState {
    Ground,
    Escape,
    Arg(AnsiArgs),
}

pub struct Terminal<'a> {
    framebuffer: Framebuffer<'a>,
    cursor_x: usize,
    cursor_y: usize,
    fg_color: u32,
    bg_color: u32,
    ansi_state: AnsiState,
}

struct Theme {
    default_fg: u32,
    default_bg: u32,
    colors: [u32; 8],
    bright_colors: [u32; 8],
}

#[allow(unused)]
const VGA_THEME: Theme = Theme {
    default_fg: 0xC0C0C0, // VGA LIGHT GRAY
    default_bg: 0x000000, // VGA BLACK
    colors: [
        0x000000, // BLACK
        0xAA0000, // RED
        0x00AA00, // GREEN
        0xAA5500, // BROWN (YELLOW in VGA palette)
        0x0000AA, // BLUE
        0xAA00AA, // MAGENTA
        0x00AAAA, // CYAN
        0xAAAAAA, // LIGHT GRAY (WHITE in VGA palette)
    ],
    bright_colors: [
        0x555555, // DARK GRAY (BRIGHT BLACK)
        0xFF5555, // LIGHT RED
        0x55FF55, // LIGHT GREEN
        0xFFFF55, // LIGHT YELLOW
        0x5555FF, // LIGHT BLUE
        0xFF55FF, // LIGHT MAGENTA
        0x55FFFF, // LIGHT CYAN
        0xFFFFFF, // WHITE (BRIGHT WHITE)
    ],
};

#[allow(unused)]
const GRUVBOX_THEME: Theme = Theme {
    default_fg: 0xEBDBB2, // GRUVBOX LIGHT FG
    default_bg: 0x282828, // GRUVBOX DARK BG
    colors: [
        0x282828, // GRUVBOX DARK BG (BLACK)
        0xCC241D, // GRUVBOX RED
        0x98971A, // GRUVBOX GREEN
        0xD79921, // GRUVBOX YELLOW
        0x458588, // GRUVBOX BLUE
        0xB16286, // GRUVBOX MAGENTA
        0x689D6A, // GRUVBOX CYAN
        0xEBDBB2, // GRUVBOX LIGHT FG (LIGHT GRAY)
    ],
    bright_colors: [
        0x665C54, // GRUVBOX DARK GRAY (BRIGHT BLACK)
        0xFB4934, // GRUVBOX LIGHT RED
        0xB8BB26, // GRUVBOX LIGHT GREEN
        0xFABD2F, // GRUVBOX LIGHT YELLOW
        0x83A598, // GRUVBOX LIGHT BLUE
        0xD3869B, // GRUVBOX LIGHT MAGENTA
        0x8EC07C, // GRUVBOX LIGHT CYAN
        0xEBDBB2, // GRUVBOX LIGHT FG (WHITE)
    ],
};

const TOKYO_NIGHT_THEME: Theme = Theme {
    default_fg: 0xC0C0C0, // TOKYO NIGHT LIGHT FG
    default_bg: 0x1A1B26, // TOKYO NIGHT DARK BG
    colors: [
        0x1A1B26, // TOKYO NIGHT DARK BG (BLACK)
        0xF7768E, // TOKYO NIGHT RED
        0x9ECE6A, // TOKYO NIGHT GREEN
        0xE0AF68, // TOKYO NIGHT YELLOW
        0x7AA2F7, // TOKYO NIGHT BLUE
        0xBB9AF7, // TOKYO NIGHT MAGENTA
        0x7DCFFF, // TOKYO NIGHT CYAN
        0xC0C0C0, // TOKYO NIGHT LIGHT FG (LIGHT GRAY)
    ],
    bright_colors: [
        0x414868, // TOKYO NIGHT DARK GRAY (BRIGHT BLACK)
        0xFF7A93, // TOKYO NIGHT LIGHT RED
        0xB9F27C, // TOKYO NIGHT LIGHT GREEN
        0xE6C384, // TOKYO NIGHT LIGHT YELLOW
        0x7AA2F7, // TOKYO NIGHT LIGHT BLUE (same as normal blue)
        0xBB9AF7, // TOKYO NIGHT LIGHT MAGENTA (same as normal magenta)
        0x7DCFFF, // TOKYO NIGHT LIGHT CYAN (same as normal cyan)
        0xFFFFFF, // WHITE (BRIGHT WHITE)
    ],
};

const DEFAULT_THEME: Theme = TOKYO_NIGHT_THEME;

impl<'a> Terminal<'a> {
    pub fn new(framebuffer: &LimineFramebuffer) -> Self {
        Terminal {
            framebuffer: Framebuffer::from(&framebuffer),
            cursor_x: 0,
            cursor_y: 0,
            fg_color: DEFAULT_THEME.default_fg,
            bg_color: DEFAULT_THEME.default_bg,
            ansi_state: AnsiState::Ground,
        }
    }

    pub fn clear(&mut self) {
        self.framebuffer.addr.fill(self.bg_color);
        self.cursor_x = 0;
        self.cursor_y = 0;
    }

    pub fn scroll(&mut self, _n: usize) {
        self.clear();
        self.cursor_y = 0;
    }

    pub fn put_char(&mut self, c: u8) {
        if c == b'\n' {
            self.cursor_x = 0;
            self.cursor_y += 10;
            if self.cursor_y + 10 > self.framebuffer.height {
                self.scroll(1);
            }
            return;
        }

        self.framebuffer.draw_glyph(
            self.cursor_x,
            self.cursor_y,
            c,
            self.fg_color,
            self.bg_color,
        );
        self.cursor_x += 8;
        if self.cursor_x + 8 > self.framebuffer.width {
            self.cursor_x = 0;
            self.cursor_y += 10;

            if self.cursor_y + 10 > self.framebuffer.height {
                self.scroll(1);
            }
        }
    }

    pub fn recv_byte(&mut self, c: u8) {
        match self.ansi_state {
            AnsiState::Ground => {
                if c == b'\x1b' {
                    self.ansi_state = AnsiState::Escape;
                } else {
                    self.put_char(c);
                    // stay in Ground
                }
            }
            AnsiState::Escape => {
                if c == b'[' {
                    self.ansi_state = AnsiState::Arg(AnsiArgs::default());
                } else {
                    self.ansi_state = AnsiState::Ground;
                }
            }
            AnsiState::Arg(ref mut args) => {
                if c >= b'0' && c <= b'9' {
                    let last = args.args[args.count - 1];
                    let new_val = last * 10 + (c - b'0') as usize;
                    args.args[args.count - 1] = new_val;
                } else if c == b';' {
                    if args.count < args.args.len() {
                        args.count += 1;
                    }
                } else if c == b'm' {
                    for param in args.args[..args.count].iter().copied() {
                        match param {
                            0 => {
                                self.fg_color = DEFAULT_THEME.default_fg;
                                self.bg_color = DEFAULT_THEME.default_bg;
                            }
                            30..=37 => {
                                let color_index = (param - 30) as usize;
                                self.fg_color = DEFAULT_THEME.colors[color_index];
                            }
                            39 => {
                                self.fg_color = DEFAULT_THEME.default_fg;
                            }
                            40..=47 => {
                                let color_index = (param - 40) as usize;
                                self.bg_color = DEFAULT_THEME.colors[color_index];
                            }
                            49 => {
                                self.bg_color = DEFAULT_THEME.default_bg;
                            }
                            90..=97 => {
                                let color_index = (param - 90) as usize;
                                self.fg_color = DEFAULT_THEME.bright_colors[color_index];
                            }
                            100..=107 => {
                                let color_index = (param - 100) as usize;
                                self.bg_color = DEFAULT_THEME.bright_colors[color_index];
                            }
                            // there are way more...
                            _ => {}
                        }
                    }
                    self.ansi_state = AnsiState::Ground;
                } else {
                    // ignore and stay in Arg
                }
            }
        }
    }

    pub fn recv_bytes(&mut self, buf: &[u8]) {
        for &b in buf {
            self.recv_byte(b);
        }
    }
}

impl Write for Terminal<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.recv_bytes(s.as_bytes());
        Ok(())
    }
}
