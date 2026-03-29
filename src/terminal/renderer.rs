use rdpfb::framebuffer::{Color, Framebuffer};
use crate::terminal::emulator::TerminalScreen;
/// Terminal renderer using cosmic-text + skrifa for colour emoji
///
/// Renders terminal cells to a framebuffer with beautiful fonts.
/// Uses cosmic-text/swash for outline glyphs, and skrifa for CBDT colour emoji.
use anyhow::Result;
use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache};
use std::collections::HashMap;

// Embed fonts directly into the binary (SIL OFL 1.1 licensed)
static FONT_REGULAR: &[u8] = include_bytes!("../../fonts/JetBrainsMonoNLNerdFontMono-Regular.ttf");
static FONT_BOLD: &[u8] = include_bytes!("../../fonts/JetBrainsMonoNLNerdFontMono-Bold.ttf");
static FONT_ITALIC: &[u8] = include_bytes!("../../fonts/JetBrainsMonoNLNerdFontMono-Italic.ttf");
static FONT_BOLD_ITALIC: &[u8] =
    include_bytes!("../../fonts/JetBrainsMonoNLNerdFontMono-BoldItalic.ttf");
static FONT_SYMBOLS: &[u8] = include_bytes!("../../fonts/SymbolsNerdFontMono-Regular.ttf");

pub struct RendererConfig {
    pub font_size: f32,
}

/// Cached cell state: (char, fg, bg, bold, italic, wide)
type CellState = (char, (u8, u8, u8), (u8, u8, u8), bool, bool, bool);

/// Parameters for glyph rendering, grouped to reduce argument count.
struct GlyphParams {
    ch: char,
    bold: bool,
    italic: bool,
    wide: bool,
    col: usize,
    row: usize,
    cell_x: usize,
    cell_y: usize,
    fg: Color,
}

struct GlyphBitmap {
    width: usize,
    left: i32,
    top: i32,
    data: Vec<u8>,
    is_color: bool,
}

pub struct TerminalRenderer {
    config: RendererConfig,
    font_system: FontSystem,
    swash_cache: SwashCache,
    pub cell_width: f32,
    pub cell_height: f32,
    glyph_cache: HashMap<(char, bool, bool), GlyphBitmap>,
    prev_cells: Vec<CellState>,
    shape_buffer: Option<Buffer>,
    /// CBDT colour emoji bitmaps: char -> RGBA image data at target size
    emoji_cache: HashMap<char, Option<(Vec<u8>, u32, u32)>>,
    /// Noto Color Emoji font data (loaded from system at runtime)
    emoji_font_data: Option<Vec<u8>>,
}

impl TerminalRenderer {
    pub fn new(config: RendererConfig) -> Result<Self> {
        let mut font_system = FontSystem::new();

        // Load embedded fonts
        font_system.db_mut().load_font_data(FONT_REGULAR.to_vec());
        font_system.db_mut().load_font_data(FONT_BOLD.to_vec());
        font_system.db_mut().load_font_data(FONT_ITALIC.to_vec());
        font_system
            .db_mut()
            .load_font_data(FONT_BOLD_ITALIC.to_vec());
        font_system.db_mut().load_font_data(FONT_SYMBOLS.to_vec());
        tracing::info!("Loaded embedded fonts: JetBrainsMonoNL NF (4 variants) + Symbols NF Mono");

        // Load Noto Color Emoji from system for colour emoji rendering
        let emoji_font_data = std::fs::read("/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf")
            .ok()
            .or_else(|| std::fs::read("/usr/share/fonts/noto/NotoColorEmoji.ttf").ok())
            .or_else(|| {
                std::fs::read("/usr/share/fonts/google-noto-emoji/NotoColorEmoji.ttf").ok()
            });
        if emoji_font_data.is_some() {
            tracing::info!("Loaded Noto Color Emoji for colour emoji rendering");
        } else {
            tracing::warn!("Noto Color Emoji not found — emoji will render as tofu");
        }

        let swash_cache = SwashCache::new();

        // Measure cell dimensions
        let metrics = Metrics::new(config.font_size, config.font_size * 1.2);
        let mut buffer = Buffer::new(&mut font_system, metrics);
        let attrs = Attrs::new().family(Family::Monospace);
        buffer.set_text(&mut font_system, "M", &attrs, Shaping::Basic);
        buffer.shape_until_scroll(&mut font_system, false);

        let mut cell_width = config.font_size * 0.6;
        if let Some(run) = buffer.layout_runs().next() {
            if let Some(glyph) = run.glyphs.first() {
                cell_width = glyph.w;
            }
        }
        let cell_height = config.font_size * 1.2;

        Ok(TerminalRenderer {
            config,
            font_system,
            swash_cache,
            cell_width,
            cell_height,
            glyph_cache: HashMap::with_capacity(512),
            prev_cells: Vec::new(),
            shape_buffer: Some(buffer),
            emoji_cache: HashMap::new(),
            emoji_font_data,
        })
    }

    pub fn render(&mut self, screen: &TerminalScreen, framebuffer: &mut Framebuffer) -> Result<()> {
        let total_cells = screen.rows * screen.cols;
        let is_full_render = self.prev_cells.len() != total_cells;

        if is_full_render {
            framebuffer.clear(Color::new(0, 0, 0));
        }

        let cw = self.cell_width;
        let ch = self.cell_height;

        for y in 0..screen.rows {
            for x in 0..screen.cols {
                let idx = y * screen.cols + x;
                if idx >= screen.cells.len() {
                    continue;
                }

                let cell = &screen.cells[idx];
                let cell_key = (
                    cell.ch,
                    cell.fg_color,
                    cell.bg_color,
                    cell.bold,
                    cell.italic,
                    cell.wide,
                );

                if !is_full_render
                    && idx < self.prev_cells.len()
                    && self.prev_cells[idx] == cell_key
                {
                    continue;
                }

                // Alacritty-style grid boundaries: floor(col * cell_w) to floor((col+1) * cell_w)
                // Adjacent cells share exact pixel edges — no gaps, no overlap
                let x_px = (x as f32 * cw).floor() as usize;
                let y_px = (y as f32 * ch).floor() as usize;
                let cell_count = if cell.wide { 2 } else { 1 };
                let x_right = ((x + cell_count) as f32 * cw).floor() as usize;
                let y_bottom = ((y + 1) as f32 * ch).floor() as usize;
                let w = x_right - x_px;
                let h = y_bottom - y_px;

                // Skip background clear for spacer cells after wide chars
                // (the wide char already cleared its 2-cell background)
                let is_after_wide =
                    x > 0 && idx > 0 && idx - 1 < screen.cells.len() && screen.cells[idx - 1].wide;

                if !is_after_wide {
                    let bg = Color::new(cell.bg_color.0, cell.bg_color.1, cell.bg_color.2);
                    framebuffer.fill_rect(x_px, y_px, w, h, bg);
                }

                if cell.ch != ' ' && cell.ch != '\0' {
                    let fg = Color::new(cell.fg_color.0, cell.fg_color.1, cell.fg_color.2);
                    self.render_glyph(
                        &GlyphParams {
                            ch: cell.ch,
                            bold: cell.bold,
                            italic: cell.italic,
                            wide: cell.wide,
                            col: x,
                            row: y,
                            cell_x: x_px,
                            cell_y: y_px,
                            fg,
                        },
                        framebuffer,
                    );
                }
            }
        }

        self.prev_cells.clear();
        self.prev_cells.reserve(total_cells);
        for cell in &screen.cells {
            self.prev_cells.push((
                cell.ch,
                cell.fg_color,
                cell.bg_color,
                cell.bold,
                cell.italic,
                cell.wide,
            ));
        }
        while self.prev_cells.len() < total_cells {
            self.prev_cells
                .push((' ', (255, 255, 255), (0, 0, 0), false, false, false));
        }

        Ok(())
    }

    fn render_glyph(&mut self, params: &GlyphParams, framebuffer: &mut Framebuffer) {
        let GlyphParams {
            ch,
            bold,
            italic,
            wide,
            col,
            row,
            cell_x,
            cell_y,
            fg,
        } = *params;
        let cp = ch as u32;

        // Box-drawing, block elements, and braille: render programmatically
        // This ensures pixel-perfect cell alignment with no sub-pixel gaps
        // Pass grid indices so we compute exact cell boundaries à la Alacritty:
        //   left  = floor(col * cell_width)
        //   right = floor((col+1) * cell_width)
        // Adjacent cells share exact pixel boundaries — no gaps.
        if (0x2500..=0x25A0).contains(&cp) || (0x2800..=0x28FF).contains(&cp) {
            self.render_box_drawing(ch, col, row, fg, framebuffer);
            return;
        }

        // Colour emoji via CBDT (cosmic-text can't rasterise these)
        if cp > 0xFF {
            if let Some(rendered) = self.try_render_emoji(ch, cell_x, cell_y, wide, framebuffer) {
                if rendered {
                    return;
                }
            }
        }

        // Unified glyph path: cosmic-text first, direct swash fallback on .notdef
        let cache_key = (ch, bold, italic);

        if !self.glyph_cache.contains_key(&cache_key) {
            let mut found = false;
            // Try cosmic-text (correct baseline/kerning for all standard glyphs)
            if let Some(bitmap) = self.rasterise_glyph(ch, bold, italic) {
                if bitmap.width > 0 {
                    self.glyph_cache.insert(cache_key, bitmap);
                    found = true;
                }
            }
            // If cosmic-text returned .notdef, try direct swash from embedded fonts
            if !found && cp > 0x7F {
                if let Some(bmp) = rasterise_from_embedded(ch, self.config.font_size) {
                    self.glyph_cache.insert(cache_key, bmp);
                    found = true;
                }
            }
            if !found {
                return;
            }
        }

        let bitmap = &self.glyph_cache[&cache_key];
        if bitmap.width == 0 {
            return;
        }

        let baseline_y = cell_y as i32 + self.config.font_size as i32;
        let glyph_x = cell_x as i32 + bitmap.left;
        let glyph_y = baseline_y - bitmap.top;
        let bytes_per_pixel = if bitmap.is_color { 4 } else { 1 };
        let row_bytes = bitmap.width * bytes_per_pixel;
        if row_bytes == 0 {
            return;
        }

        // Clip glyph rendering to cell boundaries to prevent stale pixel artefacts.
        // Without clipping, glyph overflow pixels persist when the cell content changes
        // because the background fill only covers the cell rectangle.
        let cell_w_count = if wide { 2 } else { 1 };
        let clip_right = ((col + cell_w_count) as f32 * self.cell_width).floor() as i32;
        let clip_bottom = ((row + 1) as f32 * self.cell_height).floor() as i32;
        let clip_left = cell_x as i32;
        let clip_top = cell_y as i32;
        let fw = framebuffer.width() as i32;
        let fh = framebuffer.height() as i32;

        for (img_y, row) in bitmap.data.chunks_exact(row_bytes).enumerate() {
            let fb_y = glyph_y + img_y as i32;
            if fb_y < clip_top || fb_y >= clip_bottom.min(fh) {
                continue;
            }

            for img_x in 0..bitmap.width {
                let fb_x = glyph_x + img_x as i32;
                if fb_x < clip_left || fb_x >= clip_right.min(fw) {
                    continue;
                }
                let fb_xu = fb_x as usize;
                let fb_yu = fb_y as usize;

                if bitmap.is_color {
                    let off = img_x * 4;
                    let (r, g, b, a) = (row[off], row[off + 1], row[off + 2], row[off + 3]);
                    if a == 0 {
                        continue;
                    }
                    if a == 255 {
                        framebuffer.set_pixel(fb_xu, fb_yu, Color::new(r, g, b));
                    } else {
                        let af = a as f32 / 255.0;
                        let inv = 1.0 - af;
                        if let Some(bg) = framebuffer.get_pixel(fb_xu, fb_yu) {
                            framebuffer.set_pixel(
                                fb_xu,
                                fb_yu,
                                Color::new(
                                    (r as f32 * af + bg.r as f32 * inv) as u8,
                                    (g as f32 * af + bg.g as f32 * inv) as u8,
                                    (b as f32 * af + bg.b as f32 * inv) as u8,
                                ),
                            );
                        }
                    }
                } else {
                    let alpha = row[img_x];
                    if alpha == 0 {
                        continue;
                    }
                    if alpha == 255 {
                        framebuffer.set_pixel(fb_xu, fb_yu, fg);
                    } else {
                        let a = alpha as f32 / 255.0;
                        let inv = 1.0 - a;
                        if let Some(bg) = framebuffer.get_pixel(fb_xu, fb_yu) {
                            framebuffer.set_pixel(
                                fb_xu,
                                fb_yu,
                                Color::new(
                                    (fg.r as f32 * a + bg.r as f32 * inv) as u8,
                                    (fg.g as f32 * a + bg.g as f32 * inv) as u8,
                                    (fg.b as f32 * a + bg.b as f32 * inv) as u8,
                                ),
                            );
                        }
                    }
                }
            }
        }
    }

    /// Try to render a colour emoji from CBDT bitmap data.
    /// Returns Some(true) if rendered, Some(false) if not an emoji, None if no emoji font.
    fn try_render_emoji(
        &mut self,
        ch: char,
        cell_x: usize,
        cell_y: usize,
        wide: bool,
        framebuffer: &mut Framebuffer,
    ) -> Option<bool> {
        let font_data = self.emoji_font_data.as_ref()?;

        // Check cache first
        if !self.emoji_cache.contains_key(&ch) {
            let bitmap = extract_cbdt_bitmap(font_data, ch, self.cell_height as u32);
            self.emoji_cache.insert(ch, bitmap);
        }

        let entry = self.emoji_cache.get(&ch)?;
        let (rgba, img_w, img_h) = entry.as_ref()?;

        // Scale emoji to fit cell height, width depends on wide flag
        let target_h = self.cell_height as u32;
        let target_w = if wide {
            (self.cell_width * 2.0) as u32
        } else {
            target_h
        };

        let fw = framebuffer.width();
        let fh = framebuffer.height();

        for py in 0..target_h.min(*img_h) {
            let fb_y = cell_y + py as usize;
            if fb_y >= fh {
                break;
            }

            for px in 0..target_w.min(*img_w) {
                let fb_x = cell_x + px as usize;
                if fb_x >= fw {
                    break;
                }

                // Sample from source image (nearest neighbour if sizes differ)
                let src_x = (px as usize * *img_w as usize) / target_w as usize;
                let src_y = (py as usize * *img_h as usize) / target_h as usize;
                let off = (src_y * *img_w as usize + src_x) * 4;
                if off + 3 >= rgba.len() {
                    continue;
                }

                let (r, g, b, a) = (rgba[off], rgba[off + 1], rgba[off + 2], rgba[off + 3]);
                if a == 0 {
                    continue;
                }

                if a == 255 {
                    framebuffer.set_pixel(fb_x, fb_y, Color::new(r, g, b));
                } else {
                    let af = a as f32 / 255.0;
                    let inv = 1.0 - af;
                    if let Some(bg) = framebuffer.get_pixel(fb_x, fb_y) {
                        framebuffer.set_pixel(
                            fb_x,
                            fb_y,
                            Color::new(
                                (r as f32 * af + bg.r as f32 * inv) as u8,
                                (g as f32 * af + bg.g as f32 * inv) as u8,
                                (b as f32 * af + bg.b as f32 * inv) as u8,
                            ),
                        );
                    }
                }
            }
        }

        Some(true)
    }

    /// Render box-drawing (U+2500-U+257F) and block elements (U+2580-U+259F) programmatically.
    /// Uses Alacritty-style cell boundary computation from grid indices:
    ///   left  = floor(col * cell_width),  right  = floor((col+1) * cell_width)
    ///   top   = floor(row * cell_height), bottom = floor((row+1) * cell_height)
    /// This guarantees adjacent cells share exact pixel boundaries with zero gaps.
    fn render_box_drawing(
        &mut self,
        ch: char,
        col: usize,
        row: usize,
        fg: Color,
        fb: &mut Framebuffer,
    ) {
        let cp = ch as u32;
        let x = (col as f32 * self.cell_width).floor() as usize;
        let x_right = ((col + 1) as f32 * self.cell_width).floor() as usize;
        let y = (row as f32 * self.cell_height).floor() as usize;
        let y_bottom = ((row + 1) as f32 * self.cell_height).floor() as usize;
        let cw = x_right - x;
        let ch_px = y_bottom - y;
        let mx = x + cw / 2;
        let my = y + ch_px / 2;

        match cp {
            // ─ horizontal line
            0x2500 | 0x2501 => {
                let thick = cp == 0x2501;
                let t = if thick { 3 } else { 1 };
                for dy in 0..t {
                    for px in x..x + cw {
                        fb.set_pixel(px, my - t / 2 + dy, fg);
                    }
                }
            }
            // │ vertical line
            0x2502 | 0x2503 => {
                let thick = cp == 0x2503;
                let t = if thick { 3 } else { 1 };
                for dx in 0..t {
                    for py in y..y + ch_px {
                        fb.set_pixel(mx - t / 2 + dx, py, fg);
                    }
                }
            }
            // ┌ top-left corner
            0x250C..=0x250F => {
                for px in mx..x + cw {
                    fb.set_pixel(px, my, fg);
                }
                for py in my..y + ch_px {
                    fb.set_pixel(mx, py, fg);
                }
            }
            // ┐ top-right corner
            0x2510..=0x2513 => {
                for px in x..=mx {
                    fb.set_pixel(px, my, fg);
                }
                for py in my..y + ch_px {
                    fb.set_pixel(mx, py, fg);
                }
            }
            // └ bottom-left corner
            0x2514..=0x2517 => {
                for px in mx..x + cw {
                    fb.set_pixel(px, my, fg);
                }
                for py in y..=my {
                    fb.set_pixel(mx, py, fg);
                }
            }
            // ┘ bottom-right corner
            0x2518..=0x251B => {
                for px in x..=mx {
                    fb.set_pixel(px, my, fg);
                }
                for py in y..=my {
                    fb.set_pixel(mx, py, fg);
                }
            }
            // ├ left tee
            0x251C..=0x2523 => {
                for py in y..y + ch_px {
                    fb.set_pixel(mx, py, fg);
                }
                for px in mx..x + cw {
                    fb.set_pixel(px, my, fg);
                }
            }
            // ┤ right tee
            0x2524..=0x252B => {
                for py in y..y + ch_px {
                    fb.set_pixel(mx, py, fg);
                }
                for px in x..=mx {
                    fb.set_pixel(px, my, fg);
                }
            }
            // ┬ top tee
            0x252C..=0x2533 => {
                for px in x..x + cw {
                    fb.set_pixel(px, my, fg);
                }
                for py in my..y + ch_px {
                    fb.set_pixel(mx, py, fg);
                }
            }
            // ┴ bottom tee
            0x2534..=0x253B => {
                for px in x..x + cw {
                    fb.set_pixel(px, my, fg);
                }
                for py in y..=my {
                    fb.set_pixel(mx, py, fg);
                }
            }
            // ┼ cross
            0x253C..=0x254B => {
                for px in x..x + cw {
                    fb.set_pixel(px, my, fg);
                }
                for py in y..y + ch_px {
                    fb.set_pixel(mx, py, fg);
                }
            }
            // ╌╍ dashed horizontal
            0x254C | 0x254D => {
                for px in x..x + cw {
                    if (px - x) % 4 < 2 {
                        fb.set_pixel(px, my, fg);
                    }
                }
            }
            // ╎╏ dashed vertical
            0x254E | 0x254F => {
                for py in y..y + ch_px {
                    if (py - y) % 4 < 2 {
                        fb.set_pixel(mx, py, fg);
                    }
                }
            }
            // ═ double horizontal
            0x2550 => {
                for px in x..x + cw {
                    fb.set_pixel(px, my - 1, fg);
                    fb.set_pixel(px, my + 1, fg);
                }
            }
            // ║ double vertical
            0x2551 => {
                for py in y..y + ch_px {
                    fb.set_pixel(mx - 1, py, fg);
                    fb.set_pixel(mx + 1, py, fg);
                }
            }
            // Double-line corners and tees (╔╗╚╝╠╣╦╩╬) — rendered as single-line equivalents
            0x2552..=0x256C => {
                let base = match cp {
                    0x2552..=0x2554 => 0x250C, // ┌
                    0x2555..=0x2557 => 0x2510, // ┐
                    0x2558..=0x255A => 0x2514, // └
                    0x255B..=0x255D => 0x2518, // ┘
                    0x255E..=0x2560 => 0x251C, // ├
                    0x2561..=0x2563 => 0x2524, // ┤
                    0x2564..=0x2566 => 0x252C, // ┬
                    0x2567..=0x2569 => 0x2534, // ┴
                    0x256A..=0x256C => 0x253C, // ┼
                    _ => 0x253C,
                };
                self.render_box_drawing(char::from_u32(base).unwrap_or('┼'), col, row, fg, fb);
            }
            // ▀ upper half block
            0x2580 => {
                for py in y..y + ch_px / 2 {
                    for px in x..x + cw {
                        fb.set_pixel(px, py, fg);
                    }
                }
            }
            // ▄ lower half block
            0x2584 => {
                for py in y + ch_px / 2..y + ch_px {
                    for px in x..x + cw {
                        fb.set_pixel(px, py, fg);
                    }
                }
            }
            // █ full block / ■ black square (btop uses U+25A0 for progress bars)
            0x2588 | 0x25A0 => {
                for py in y..y + ch_px {
                    for px in x..x + cw {
                        fb.set_pixel(px, py, fg);
                    }
                }
            }
            // ▌ left half block
            0x258C => {
                for py in y..y + ch_px {
                    for px in x..x + cw / 2 {
                        fb.set_pixel(px, py, fg);
                    }
                }
            }
            // ▐ right half block
            0x2590 => {
                for py in y..y + ch_px {
                    for px in x + cw / 2..x + cw {
                        fb.set_pixel(px, py, fg);
                    }
                }
            }
            // ░ light shade (25%)
            0x2591 => {
                for py in y..y + ch_px {
                    for px in x..x + cw {
                        if (px + py) % 4 == 0 {
                            fb.set_pixel(px, py, fg);
                        }
                    }
                }
            }
            // ▒ medium shade (50%)
            0x2592 => {
                for py in y..y + ch_px {
                    for px in x..x + cw {
                        if (px + py) % 2 == 0 {
                            fb.set_pixel(px, py, fg);
                        }
                    }
                }
            }
            // ▓ dark shade (75%)
            0x2593 => {
                for py in y..y + ch_px {
                    for px in x..x + cw {
                        if (px + py) % 4 != 0 {
                            fb.set_pixel(px, py, fg);
                        }
                    }
                }
            }
            // ▁▂▃▅▆▇ fractional lower blocks
            0x2581..=0x2587 => {
                let eighths = (cp - 0x2580) as usize; // 1-7
                let block_h = ch_px * eighths / 8;
                for py in (y + ch_px - block_h)..y + ch_px {
                    for px in x..x + cw {
                        fb.set_pixel(px, py, fg);
                    }
                }
            }
            // ▉▊▋▍▎▏ fractional left blocks
            0x2589..=0x258F => {
                let eighths = (8 - (cp - 0x2588)) as usize; // 7 down to 1
                let block_w = cw * eighths / 8;
                for py in y..y + ch_px {
                    for px in x..x + block_w {
                        fb.set_pixel(px, py, fg);
                    }
                }
            }
            // ▔ upper one-eighth block
            0x2594 => {
                let h = ch_px / 8;
                for py in y..y + h.max(1) {
                    for px in x..x + cw {
                        fb.set_pixel(px, py, fg);
                    }
                }
            }
            // ▕ right one-eighth block
            0x2595 => {
                let w = cw / 8;
                for py in y..y + ch_px {
                    for px in (x + cw - w.max(1))..x + cw {
                        fb.set_pixel(px, py, fg);
                    }
                }
            }
            // ╭ rounded top-left
            0x256D => {
                for px in mx..x + cw {
                    fb.set_pixel(px, my, fg);
                }
                for py in my..y + ch_px {
                    fb.set_pixel(mx, py, fg);
                }
            }
            // ╮ rounded top-right
            0x256E => {
                for px in x..=mx {
                    fb.set_pixel(px, my, fg);
                }
                for py in my..y + ch_px {
                    fb.set_pixel(mx, py, fg);
                }
            }
            // ╯ rounded bottom-right
            0x256F => {
                for px in x..=mx {
                    fb.set_pixel(px, my, fg);
                }
                for py in y..=my {
                    fb.set_pixel(mx, py, fg);
                }
            }
            // ╰ rounded bottom-left
            0x2570 => {
                for px in mx..x + cw {
                    fb.set_pixel(px, my, fg);
                }
                for py in y..=my {
                    fb.set_pixel(mx, py, fg);
                }
            }
            // ╱ diagonal
            0x2571 => {
                for i in 0..ch_px.min(cw) {
                    let px = x + cw - 1 - i * cw / ch_px;
                    let py = y + i;
                    if px < x + cw && py < y + ch_px {
                        fb.set_pixel(px, py, fg);
                    }
                }
            }
            // ╲ diagonal
            0x2572 => {
                for i in 0..ch_px.min(cw) {
                    let px = x + i * cw / ch_px;
                    let py = y + i;
                    if px < x + cw && py < y + ch_px {
                        fb.set_pixel(px, py, fg);
                    }
                }
            }
            // ╳ cross diagonal
            0x2573 => {
                self.render_box_drawing('\u{2571}', col, row, fg, fb);
                self.render_box_drawing('\u{2572}', col, row, fg, fb);
            }
            // Braille patterns U+2800-U+28FF
            // Each braille character is a 2x4 dot grid
            // Bit layout: dot1(0x01) dot2(0x02) dot3(0x04) dot4(0x40)
            //             dot5(0x08) dot6(0x10) dot7(0x20) dot8(0x80)
            0x2800..=0x28FF => {
                let bits = (cp - 0x2800) as u8;
                let dot_w = cw / 2;
                let dot_h = ch_px / 4;
                // Dot positions: column 0 (left), column 1 (right)
                // Row 0: bits 0,3  Row 1: bits 1,4  Row 2: bits 2,5  Row 3: bits 6,7
                let dots: [(u8, usize, usize); 8] = [
                    (0x01, 0, 0), // dot 1: col 0, row 0
                    (0x02, 0, 1), // dot 2: col 0, row 1
                    (0x04, 0, 2), // dot 3: col 0, row 2
                    (0x08, 1, 0), // dot 4: col 1, row 0
                    (0x10, 1, 1), // dot 5: col 1, row 1
                    (0x20, 1, 2), // dot 6: col 1, row 2
                    (0x40, 0, 3), // dot 7: col 0, row 3
                    (0x80, 1, 3), // dot 8: col 1, row 3
                ];
                for (bit, col, row) in &dots {
                    if bits & bit != 0 {
                        let dx = x + col * dot_w;
                        let dy = y + row * dot_h;
                        // Fill most of the dot cell for visibility
                        let dw = (dot_w as f32 * 0.7) as usize;
                        let dh = (dot_h as f32 * 0.7) as usize;
                        let dw = dw.max(2);
                        let dh = dh.max(2);
                        let ox = (dot_w - dw) / 2; // centre the dot
                        let oy = (dot_h - dh) / 2;
                        for py in (dy + oy)..(dy + oy + dh) {
                            for px in (dx + ox)..(dx + ox + dw) {
                                if px < x + cw && py < y + ch_px {
                                    fb.set_pixel(px, py, fg);
                                }
                            }
                        }
                    }
                }
            }
            // Anything else in range — fall back to font glyph
            _ => {
                // Use swash rasterisation for unhandled box-drawing chars
                let cache_key = (ch, false, false);
                if !self.glyph_cache.contains_key(&cache_key) {
                    if let Some(bmp) = rasterise_from_embedded(ch, self.config.font_size) {
                        self.glyph_cache.insert(cache_key, bmp);
                    } else {
                        return;
                    }
                }
                let bitmap = &self.glyph_cache[&cache_key];
                if bitmap.width == 0 {
                    return;
                }
                // Force cell-aligned positioning, clipped to cell bounds
                let glyph_x = x as i32;
                let glyph_y = y as i32;
                let clip_right = (x + cw) as i32;
                let clip_bottom = (y + ch_px) as i32;
                for (img_y, row) in bitmap.data.chunks_exact(bitmap.width).enumerate() {
                    let fby = glyph_y + img_y as i32;
                    if fby < glyph_y || fby >= clip_bottom {
                        continue;
                    }
                    for (img_x, &alpha) in row.iter().enumerate() {
                        if alpha == 0 {
                            continue;
                        }
                        let fbx = glyph_x + img_x as i32;
                        if fbx < glyph_x || fbx >= clip_right {
                            continue;
                        }
                        if alpha == 255 {
                            fb.set_pixel(fbx as usize, fby as usize, fg);
                        } else {
                            let a = alpha as f32 / 255.0;
                            let inv = 1.0 - a;
                            if let Some(bg) = fb.get_pixel(fbx as usize, fby as usize) {
                                fb.set_pixel(
                                    fbx as usize,
                                    fby as usize,
                                    Color::new(
                                        (fg.r as f32 * a + bg.r as f32 * inv) as u8,
                                        (fg.g as f32 * a + bg.g as f32 * inv) as u8,
                                        (fg.b as f32 * a + bg.b as f32 * inv) as u8,
                                    ),
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    fn rasterise_glyph(&mut self, ch: char, bold: bool, italic: bool) -> Option<GlyphBitmap> {
        let metrics = Metrics::new(self.config.font_size, self.config.font_size * 1.2);

        let mut buffer = self
            .shape_buffer
            .take()
            .unwrap_or_else(|| Buffer::new(&mut self.font_system, metrics));
        buffer.set_metrics(&mut self.font_system, metrics);

        // For nerd font PUA characters, try Symbols Nerd Font first
        let cp = ch as u32;
        let is_pua = (0xE000..=0xF8FF).contains(&cp) || (0xF0000..=0xFFFFF).contains(&cp);
        let families_to_try: &[Family] = if is_pua {
            &[
                Family::Name("Symbols Nerd Font Mono"),
                Family::Name("JetBrainsMonoNL Nerd Font Mono"),
                Family::Monospace,
            ]
        } else {
            &[Family::Monospace]
        };

        let mut result = None;

        for family in families_to_try {
            let mut attrs = Attrs::new().family(*family);
            if bold {
                attrs = attrs.weight(cosmic_text::Weight::BOLD);
            }
            if italic {
                attrs = attrs.style(cosmic_text::Style::Italic);
            }

            let s = ch.to_string();
            buffer.set_text(&mut self.font_system, &s, &attrs, Shaping::Basic);
            buffer.shape_until_scroll(&mut self.font_system, false);

            if let Some(run) = buffer.layout_runs().next() {
                if let Some(glyph) = run.glyphs.first() {
                    // Skip .notdef (glyph_id 0) — this is the tofu box
                    if glyph.glyph_id != 0 {
                        let physical = glyph.physical((0.0, self.config.font_size), 1.0);
                        if let Some(image) = self
                            .swash_cache
                            .get_image(&mut self.font_system, physical.cache_key)
                        {
                            if image.placement.width > 0 && image.placement.height > 0 {
                                use cosmic_text::SwashContent;
                                let is_color = matches!(
                                    image.content,
                                    SwashContent::Color | SwashContent::SubpixelMask
                                );
                                result = Some(GlyphBitmap {
                                    width: image.placement.width as usize,
                                    left: image.placement.left,
                                    top: image.placement.top,
                                    data: image.data.clone(),
                                    is_color,
                                });
                            }
                        }
                    }
                }
            }

            if result.is_some() {
                break;
            }
        }

        self.shape_buffer = Some(buffer);
        result
    }
}

/// Rasterise a PUA glyph directly from embedded font data using swash.
/// Bypasses cosmic-text entirely — checks JetBrains Mono NF first, then Symbols NF.
fn rasterise_from_embedded(ch: char, font_size: f32) -> Option<GlyphBitmap> {
    // Try Symbols NF first (properly sized PUA glyphs), then JetBrains Mono NF as fallback.
    // Some fonts have small PUA glyphs relative to em square — try larger sizes if needed.
    for font_data in &[FONT_SYMBOLS, FONT_REGULAR] {
        // Try at the normal font size first
        if let Some(bmp) = rasterise_char_from_font(font_data, ch, font_size) {
            // If the glyph is too small (less than 60% of cell height), re-render larger
            let cell_h = font_size * 1.2;
            if (bmp.data.len() / bmp.width.max(1)) < (cell_h * 0.6) as usize && bmp.width > 0 {
                // Scale up to fill the cell
                let scale = cell_h / (bmp.data.len() / bmp.width.max(1)) as f32;
                let scaled_size = font_size * scale * 0.85; // 85% to leave margin
                if let Some(bigger) = rasterise_char_from_font(font_data, ch, scaled_size) {
                    return Some(bigger);
                }
            }
            return Some(bmp);
        }
    }
    None
}

fn rasterise_char_from_font(font_data: &[u8], ch: char, font_size: f32) -> Option<GlyphBitmap> {
    use swash::FontRef as SwashFontRef;
    use swash::scale::{Render, ScaleContext, Source};
    use swash::zeno::{Format, Vector};

    let font = SwashFontRef::from_index(font_data, 0)?;
    let charmap = font.charmap();
    let glyph_id = charmap.map(ch);
    if glyph_id == 0 {
        return None;
    } // .notdef

    let mut context = ScaleContext::new();
    let mut scaler = context.builder(font).size(font_size).build();

    let render_result = Render::new(&[Source::Outline])
        .format(Format::Alpha)
        .offset(Vector::new(0.0, 0.0))
        .render(&mut scaler, glyph_id);

    let image = render_result?;

    if image.placement.width == 0 || image.placement.height == 0 {
        return None;
    }

    Some(GlyphBitmap {
        width: image.placement.width as usize,
        left: image.placement.left,
        top: image.placement.top,
        data: image.data,
        is_color: false,
    })
}

/// Extract a colour bitmap from a font file for a given character.
/// Uses skrifa's BitmapStrikes API to read CBDT/CBLC data.
/// Returns RGBA pixel data and dimensions, or None if not found.
fn extract_cbdt_bitmap(
    font_data: &[u8],
    ch: char,
    target_size: u32,
) -> Option<(Vec<u8>, u32, u32)> {
    use skrifa::FontRef;
    use skrifa::MetadataProvider;
    use skrifa::bitmap::BitmapStrikes;
    use skrifa::instance::Size;

    let font = FontRef::from_index(font_data, 0).ok()?;

    // Get glyph ID
    let charmap = font.charmap();
    let glyph_id = charmap.map(ch)?;

    // Get bitmap strikes
    let strikes = BitmapStrikes::new(&font);
    if strikes.is_empty() {
        return None;
    }

    // Find best strike for target size, then get glyph
    let size = Size::new(target_size as f32);
    let bitmap_glyph = strikes.glyph_for_size(size, glyph_id)?;

    // Decode bitmap data
    use skrifa::bitmap::BitmapData;
    match &bitmap_glyph.data {
        BitmapData::Png(png_data) => {
            if let Ok(img) = image::load_from_memory(png_data) {
                let rgba = img.to_rgba8();
                let (w, h) = (rgba.width(), rgba.height());
                return Some((rgba.into_raw(), w, h));
            }
        }
        BitmapData::Bgra(bgra_data) => {
            // Convert BGRA to RGBA
            let w = bitmap_glyph.width;
            let h = bgra_data.len() as u32 / (w * 4);
            let mut rgba = Vec::with_capacity(bgra_data.len());
            for pixel in bgra_data.chunks_exact(4) {
                rgba.push(pixel[2]); // R
                rgba.push(pixel[1]); // G
                rgba.push(pixel[0]); // B
                rgba.push(pixel[3]); // A
            }
            return Some((rgba, w, h));
        }
        _ => {}
    }

    None
}
