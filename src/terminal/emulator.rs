use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags as CellFlags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi;
/// Terminal emulator using alacritty_terminal
///
/// Full VTE-compatible terminal emulator with alternate screen buffer,
/// scroll regions, 256 colours, true colour, and damage tracking.
use anyhow::Result;

/// No-op event listener — we don't need terminal events
struct NullListener;
impl EventListener for NullListener {
    fn send_event(&self, _event: Event) {}
}

/// Terminal dimensions for alacritty
struct TermSize {
    cols: usize,
    lines: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// Terminal emulator backed by alacritty_terminal
pub struct TerminalEmulator {
    term: Term<NullListener>,
    parser: ansi::Processor,
    rows: usize,
    cols: usize,
}

impl TerminalEmulator {
    pub fn new(cols: usize, rows: usize) -> Result<Self> {
        let size = TermSize { cols, lines: rows };
        let config = Config::default();
        let term = Term::new(config, &size, NullListener);
        let parser = ansi::Processor::new();

        Ok(TerminalEmulator {
            term,
            parser,
            rows,
            cols,
        })
    }

    /// Feed PTY output bytes into the terminal emulator
    pub fn process_output(&mut self, data: &[u8]) -> Result<()> {
        self.parser.advance(&mut self.term, data);
        Ok(())
    }

    /// Get the current screen state for rendering
    pub fn get_screen(&self) -> Result<TerminalScreen> {
        let grid = self.term.grid();
        let mut cells = Vec::with_capacity(self.rows * self.cols);
        let colors = self.term.colors();

        for line_idx in 0..self.rows {
            let line = grid.display_offset() as i32 + line_idx as i32;
            let row = &grid[alacritty_terminal::index::Line(line)];

            for col_idx in 0..self.cols {
                let col = alacritty_terminal::index::Column(col_idx);
                let cell = &row[col];

                // Skip wide char spacer cells — the wide char itself handles both columns
                let is_spacer = cell.flags.contains(CellFlags::WIDE_CHAR_SPACER)
                    || cell.flags.contains(CellFlags::LEADING_WIDE_CHAR_SPACER);
                let is_wide = cell.flags.contains(CellFlags::WIDE_CHAR);

                let ch = if is_spacer || cell.c == '\0' {
                    ' ' // spacer or null — will be skipped by renderer
                } else {
                    cell.c
                };
                let fg_color = resolve_color(&cell.fg, colors, true);
                let bg_color = resolve_color(&cell.bg, colors, false);
                let bold = cell.flags.contains(CellFlags::BOLD);
                let italic = cell.flags.contains(CellFlags::ITALIC);

                cells.push(TerminalCell {
                    ch,
                    fg_color,
                    bg_color,
                    bold,
                    italic,
                    wide: is_wide,
                });
            }
        }

        Ok(TerminalScreen {
            cells,
            cols: self.cols,
            rows: self.rows,
        })
    }
}

/// Resolve a vte Color to RGB
fn resolve_color(color: &ansi::Color, colors: &Colors, is_fg: bool) -> (u8, u8, u8) {
    match color {
        ansi::Color::Named(named) => {
            let idx = *named as usize;
            if let Some(rgb) = colors[idx] {
                (rgb.r, rgb.g, rgb.b)
            } else {
                default_named_color(*named, is_fg)
            }
        }
        ansi::Color::Spec(rgb) => (rgb.r, rgb.g, rgb.b),
        ansi::Color::Indexed(idx) => {
            if let Some(rgb) = colors[*idx as usize] {
                (rgb.r, rgb.g, rgb.b)
            } else {
                palette_index_to_rgb(*idx)
            }
        }
    }
}

fn default_named_color(named: ansi::NamedColor, is_fg: bool) -> (u8, u8, u8) {
    use ansi::NamedColor::*;
    match named {
        Black => (0, 0, 0),
        Red => (205, 0, 0),
        Green => (0, 205, 0),
        Yellow => (205, 205, 0),
        Blue => (0, 0, 238),
        Magenta => (205, 0, 205),
        Cyan => (0, 205, 205),
        White => (229, 229, 229),
        BrightBlack => (127, 127, 127),
        BrightRed => (255, 0, 0),
        BrightGreen => (0, 255, 0),
        BrightYellow => (255, 255, 0),
        BrightBlue => (92, 92, 255),
        BrightMagenta => (255, 0, 255),
        BrightCyan => (0, 255, 255),
        BrightWhite => (255, 255, 255),
        Foreground => {
            if is_fg {
                (255, 255, 255)
            } else {
                (0, 0, 0)
            }
        }
        Background => {
            if is_fg {
                (255, 255, 255)
            } else {
                (0, 0, 0)
            }
        }
        Cursor | DimForeground | BrightForeground | DimBlack | DimRed | DimGreen | DimYellow
        | DimBlue | DimMagenta | DimCyan | DimWhite => {
            if is_fg {
                (200, 200, 200)
            } else {
                (0, 0, 0)
            }
        }
    }
}

fn palette_index_to_rgb(idx: u8) -> (u8, u8, u8) {
    match idx {
        0 => (0, 0, 0),
        1 => (205, 0, 0),
        2 => (0, 205, 0),
        3 => (205, 205, 0),
        4 => (0, 0, 238),
        5 => (205, 0, 205),
        6 => (0, 205, 205),
        7 => (229, 229, 229),
        8 => (127, 127, 127),
        9 => (255, 0, 0),
        10 => (0, 255, 0),
        11 => (255, 255, 0),
        12 => (92, 92, 255),
        13 => (255, 0, 255),
        14 => (0, 255, 255),
        15 => (255, 255, 255),
        16..=231 => {
            let i = idx - 16;
            ((i / 36) * 51, ((i % 36) / 6) * 51, (i % 6) * 51)
        }
        232..=255 => {
            let g = 8 + (idx - 232) * 10;
            (g, g, g)
        }
    }
}

// ===== Screen and cell types =====

pub struct TerminalScreen {
    pub cells: Vec<TerminalCell>,
    pub cols: usize,
    pub rows: usize,
}

#[derive(Debug, Clone)]
pub struct TerminalCell {
    pub ch: char,
    pub fg_color: (u8, u8, u8),
    pub bg_color: (u8, u8, u8),
    pub bold: bool,
    pub italic: bool,
    pub wide: bool,
}

impl Default for TerminalCell {
    fn default() -> Self {
        TerminalCell {
            ch: ' ',
            fg_color: (255, 255, 255),
            bg_color: (0, 0, 0),
            bold: false,
            italic: false,
            wide: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_creates_correct_dimensions() {
        let emu = TerminalEmulator::new(80, 24).unwrap();
        let screen = emu.get_screen().unwrap();
        assert_eq!(screen.cols, 80);
        assert_eq!(screen.rows, 24);
        assert_eq!(screen.cells.len(), 80 * 24);
    }

    #[test]
    fn test_process_output_plain_ascii() {
        let mut emu = TerminalEmulator::new(80, 24).unwrap();
        emu.process_output(b"Hello").unwrap();
        let screen = emu.get_screen().unwrap();
        assert_eq!(screen.cells[0].ch, 'H');
        assert_eq!(screen.cells[1].ch, 'e');
        assert_eq!(screen.cells[2].ch, 'l');
        assert_eq!(screen.cells[3].ch, 'l');
        assert_eq!(screen.cells[4].ch, 'o');
        // Rest should be spaces
        assert_eq!(screen.cells[5].ch, ' ');
    }

    #[test]
    fn test_ansi_fg_color() {
        let mut emu = TerminalEmulator::new(80, 24).unwrap();
        // ESC[31m = red foreground, then 'X'
        emu.process_output(b"\x1b[31mX").unwrap();
        let screen = emu.get_screen().unwrap();
        assert_eq!(screen.cells[0].ch, 'X');
        // Red foreground: should be (205, 0, 0) or similar non-white
        assert_ne!(screen.cells[0].fg_color, (255, 255, 255));
    }

    #[test]
    fn test_ansi_bg_color() {
        let mut emu = TerminalEmulator::new(80, 24).unwrap();
        // ESC[42m = green background, then 'X'
        emu.process_output(b"\x1b[42mX").unwrap();
        let screen = emu.get_screen().unwrap();
        assert_eq!(screen.cells[0].ch, 'X');
        // Green background: should be non-black
        assert_ne!(screen.cells[0].bg_color, (0, 0, 0));
    }

    #[test]
    fn test_ansi_bold_flag() {
        let mut emu = TerminalEmulator::new(80, 24).unwrap();
        // ESC[1m = bold
        emu.process_output(b"\x1b[1mB").unwrap();
        let screen = emu.get_screen().unwrap();
        assert_eq!(screen.cells[0].ch, 'B');
        assert!(screen.cells[0].bold);
    }

    #[test]
    fn test_ansi_italic_flag() {
        let mut emu = TerminalEmulator::new(80, 24).unwrap();
        // ESC[3m = italic
        emu.process_output(b"\x1b[3mI").unwrap();
        let screen = emu.get_screen().unwrap();
        assert_eq!(screen.cells[0].ch, 'I');
        assert!(screen.cells[0].italic);
    }

    #[test]
    fn test_wide_character() {
        let mut emu = TerminalEmulator::new(80, 24).unwrap();
        // CJK character (U+4E16 = '世')
        emu.process_output("世".as_bytes()).unwrap();
        let screen = emu.get_screen().unwrap();
        assert_eq!(screen.cells[0].ch, '世');
        assert!(screen.cells[0].wide);
        // The next cell should be a spacer (rendered as space)
        assert_eq!(screen.cells[1].ch, ' ');
    }

    #[test]
    fn test_terminal_cell_default() {
        let cell = TerminalCell::default();
        assert_eq!(cell.ch, ' ');
        assert_eq!(cell.fg_color, (255, 255, 255));
        assert_eq!(cell.bg_color, (0, 0, 0));
        assert!(!cell.bold);
        assert!(!cell.italic);
        assert!(!cell.wide);
    }

    #[test]
    fn test_palette_index_to_rgb_standard_colors() {
        // Black
        assert_eq!(palette_index_to_rgb(0), (0, 0, 0));
        // Red
        assert_eq!(palette_index_to_rgb(1), (205, 0, 0));
        // Green
        assert_eq!(palette_index_to_rgb(2), (0, 205, 0));
        // White
        assert_eq!(palette_index_to_rgb(7), (229, 229, 229));
        // Bright red
        assert_eq!(palette_index_to_rgb(9), (255, 0, 0));
        // Bright white
        assert_eq!(palette_index_to_rgb(15), (255, 255, 255));
    }

    #[test]
    fn test_palette_index_to_rgb_6x6x6_cube() {
        // Index 16 = (0,0,0) in the cube
        assert_eq!(palette_index_to_rgb(16), (0, 0, 0));
        // Index 21 = (0,0,5*51) = (0, 0, 255)
        assert_eq!(palette_index_to_rgb(21), (0, 0, 255));
        // Index 196 = (5*51, 0, 0) = (255, 0, 0)
        assert_eq!(palette_index_to_rgb(196), (255, 0, 0));
        // Index 231 = (5*51, 5*51, 5*51) = (255, 255, 255)
        assert_eq!(palette_index_to_rgb(231), (255, 255, 255));
    }

    #[test]
    fn test_palette_index_to_rgb_greyscale() {
        // Index 232 = 8 + 0*10 = 8
        assert_eq!(palette_index_to_rgb(232), (8, 8, 8));
        // Index 255 = 8 + 23*10 = 238
        assert_eq!(palette_index_to_rgb(255), (238, 238, 238));
        // Index 244 = 8 + 12*10 = 128
        assert_eq!(palette_index_to_rgb(244), (128, 128, 128));
    }

    #[test]
    fn test_default_named_color_fg() {
        use alacritty_terminal::vte::ansi::NamedColor;
        assert_eq!(default_named_color(NamedColor::Black, true), (0, 0, 0));
        assert_eq!(default_named_color(NamedColor::Red, true), (205, 0, 0));
        assert_eq!(
            default_named_color(NamedColor::BrightWhite, true),
            (255, 255, 255)
        );
        // Foreground as fg
        assert_eq!(
            default_named_color(NamedColor::Foreground, true),
            (255, 255, 255)
        );
        // Background as fg
        assert_eq!(
            default_named_color(NamedColor::Background, true),
            (255, 255, 255)
        );
    }

    #[test]
    fn test_default_named_color_bg() {
        use alacritty_terminal::vte::ansi::NamedColor;
        // Foreground as bg
        assert_eq!(
            default_named_color(NamedColor::Foreground, false),
            (0, 0, 0)
        );
        // Background as bg
        assert_eq!(
            default_named_color(NamedColor::Background, false),
            (0, 0, 0)
        );
        // Dim colours as bg
        assert_eq!(default_named_color(NamedColor::DimRed, false), (0, 0, 0));
    }
}
