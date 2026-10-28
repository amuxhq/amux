//! Deterministic PNG captures of the terminal's vocabulary components:
//! the same buffers the component goldens compare, rasterized with
//! vendored fonts so a reviewer sees what the text goldens describe.

pub mod stories;

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use fontdue::{Font, FontSettings};
use ratatui::buffer::{Buffer, Cell};
use ratatui::style::{Color, Modifier};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tui::vocabulary::components;
use tui::{ColorMode, Theme};

/// Pixel width of one terminal cell.
pub const CELL_WIDTH: u32 = 10;
/// Pixel height of one terminal cell. Glyphs are clipped to their own cell,
/// so the cell must contain the font's whole vertical extent or every row
/// loses the tips of its ascenders and the tails of its descenders. At
/// `FONT_SIZE`, JetBrains Mono asks for 16.32px above the baseline and 4.80px
/// below it, which needs 22 rather than the 20 an even 16px cell suggests.
pub const CELL_HEIGHT: u32 = 22;

const FONT_SIZE: f32 = 16.0;
/// Distance from the top of a cell to the text baseline, leaving whole pixels
/// for the ascent above it and the descent below it.
const BASELINE: i32 = 17;
const REGULAR_FONT: &[u8] = include_bytes!("../assets/JetBrainsMono-Regular.ttf");
const BOLD_FONT: &[u8] = include_bytes!("../assets/JetBrainsMono-Bold.ttf");
const ITALIC_FONT: &[u8] = include_bytes!("../assets/JetBrainsMono-Italic.ttf");
const BOLD_ITALIC_FONT: &[u8] = include_bytes!("../assets/JetBrainsMono-BoldItalic.ttf");
const SYMBOL_FONT: &[u8] = include_bytes!("../assets/DejaVuSans.ttf");

/// RGB pixels produced from a terminal buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// One PNG recorded in a directory manifest.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ManifestEntry {
    pub state: String,
    pub theme: String,
    pub color: String,
    pub viewport: [u16; 2],
    pub pixels: [u32; 2],
    pub file: String,
    pub sha256: String,
}

/// One completed render-set invocation.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ManifestSet {
    pub name: String,
    pub files: Vec<String>,
}

/// The capture record stored beside rendered files.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Manifest {
    pub entries: Vec<ManifestEntry>,
    #[serde(default)]
    pub sets: Vec<ManifestSet>,
}

#[derive(Debug, Error)]
pub enum ShotError {
    #[error("UnknownState({0})")]
    UnknownState(String),
    #[error("UnknownSet({0})")]
    UnknownSet(String),
    #[error("theme error: {0}")]
    Theme(#[from] tui::ThemeError),
    #[error("render error: {0}")]
    Render(String),
    #[error("font error: {0}")]
    Font(String),
    #[error("PNG error: {0}")]
    Encode(String),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("verification failed: {0}")]
    Verify(String),
    #[error("not a report's frame: {0}")]
    Frame(String),
}

struct Fonts {
    regular: Font,
    bold: Font,
    italic: Font,
    bold_italic: Font,
    symbols: Font,
}

impl Fonts {
    fn shared() -> Result<&'static Self, ShotError> {
        // Every capture uses the same embedded faces. Parsing them for each
        // animation frame dominates the raster work and cannot change a pixel.
        static FONTS: OnceLock<Result<Fonts, String>> = OnceLock::new();
        FONTS
            .get_or_init(Self::load)
            .as_ref()
            .map_err(|error| ShotError::Font(error.clone()))
    }

    fn load() -> Result<Self, String> {
        fn parse(bytes: &'static [u8], face: &str) -> Result<Font, String> {
            Font::from_bytes(bytes, FontSettings::default())
                .map_err(|error| format!("{face}: {error}"))
        }

        Ok(Self {
            regular: parse(REGULAR_FONT, "regular")?,
            bold: parse(BOLD_FONT, "bold")?,
            italic: parse(ITALIC_FONT, "italic")?,
            bold_italic: parse(BOLD_ITALIC_FONT, "bold italic")?,
            symbols: parse(SYMBOL_FONT, "symbols fallback")?,
        })
    }

    fn for_cell(&self, cell: &Cell) -> &Font {
        match (
            cell.modifier.contains(Modifier::BOLD),
            cell.modifier.contains(Modifier::ITALIC),
        ) {
            (true, true) => &self.bold_italic,
            (true, false) => &self.bold,
            (false, true) => &self.italic,
            (false, false) => &self.regular,
        }
    }

    fn for_character(&self, cell: &Cell, character: char) -> &Font {
        let primary = self.for_cell(cell);
        if primary.lookup_glyph_index(character) == 0 {
            &self.symbols
        } else {
            primary
        }
    }
}

/// Render every vocabulary component in `theme` into `dir`, one PNG each
/// named `<component>.<theme_label>.png`, and record them as the set
/// `vocabulary`. Returns the entries written.
pub fn render_vocabulary(
    dir: &Path,
    theme: Theme,
    theme_label: &str,
) -> Result<Vec<ManifestEntry>, ShotError> {
    fs::create_dir_all(dir)?;
    let mut entries = Vec::new();
    for component in components(theme) {
        let file = format!("{}.{theme_label}.png", component.name);
        entries.push(render_to_path(
            component.name,
            &component.buffer,
            theme,
            theme_label,
            &dir.join(&file),
        )?);
    }
    append_set(
        dir,
        &format!("vocabulary.{theme_label}"),
        entries.iter().map(|entry| entry.file.clone()).collect(),
    )?;
    Ok(entries)
}

/// Every component's name and what it shows, for an index of captures.
pub fn vocabulary_index() -> Vec<(&'static str, &'static str)> {
    components(Theme::default())
        .into_iter()
        .map(|component| (component.name, component.shows))
        .collect()
}

/// Rasterize, encode, and record one capture in the manifest beside `out`.
pub fn render_to_path(
    state: &str,
    buffer: &Buffer,
    theme: Theme,
    theme_label: &str,
    out: &Path,
) -> Result<ManifestEntry, ShotError> {
    let raster = rasterize(buffer, theme)?;
    write_png(&raster, out)?;
    let bytes = fs::read(out)?;
    let file = out
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            ShotError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "PNG output needs a file name",
            ))
        })?
        .to_string();
    let entry = ManifestEntry {
        state: state.to_string(),
        theme: theme_label.to_string(),
        color: color_mode_name(theme.mode).to_string(),
        viewport: [buffer.area.width, buffer.area.height],
        pixels: [raster.width, raster.height],
        file,
        sha256: hex_digest(&bytes),
    };
    append_entry(
        out.parent().unwrap_or_else(|| Path::new(".")),
        entry.clone(),
    )?;
    Ok(entry)
}

/// A report's `frame.json`, as the terminal client writes it: rows of
/// runs, each a stretch of cells in one style from its first column.
#[derive(Deserialize)]
struct ReportFrame {
    width: u16,
    height: u16,
    default: DefaultInk,
    rows: Vec<Vec<Run>>,
}

#[derive(Deserialize)]
struct DefaultInk {
    fg: String,
    bg: String,
}

#[derive(Deserialize)]
struct Run {
    x: u16,
    text: String,
    fg: String,
    bg: String,
    #[serde(default)]
    styles: Vec<String>,
}

/// The cells a report froze, rebuilt from its `frame.json`. The terminal's
/// own ink and ground take the values the report recorded for them, and its
/// numbered colours the conventional ones.
pub fn report_frame(json: &str) -> Result<Buffer, ShotError> {
    let frame: ReportFrame =
        serde_json::from_str(json).map_err(|error| ShotError::Frame(error.to_string()))?;
    let color = |name: &str, default: &str| -> Result<Color, ShotError> {
        let name = if name == "default" { default } else { name };
        if let Some(index) = name.strip_prefix("ansi:") {
            return index
                .parse()
                .map(Color::Indexed)
                .map_err(|_| ShotError::Frame(format!("bad colour {name}")));
        }
        let hex = name
            .strip_prefix('#')
            .filter(|hex| hex.len() == 6)
            .and_then(|hex| u32::from_str_radix(hex, 16).ok())
            .ok_or_else(|| ShotError::Frame(format!("bad colour {name}")))?;
        Ok(Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8))
    };
    let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, frame.width, frame.height));
    let ground = color(&frame.default.bg, &frame.default.bg)?;
    let ink = color(&frame.default.fg, &frame.default.fg)?;
    buffer.set_style(
        buffer.area,
        ratatui::style::Style::default().fg(ink).bg(ground),
    );
    for (y, runs) in frame.rows.iter().enumerate().take(frame.height as usize) {
        for run in runs {
            let mut style = ratatui::style::Style::default()
                .fg(color(&run.fg, &frame.default.fg)?)
                .bg(color(&run.bg, &frame.default.bg)?);
            for name in &run.styles {
                style = style.add_modifier(match name.as_str() {
                    "bold" => Modifier::BOLD,
                    "dim" => Modifier::DIM,
                    "italic" => Modifier::ITALIC,
                    "underlined" => Modifier::UNDERLINED,
                    "reversed" => Modifier::REVERSED,
                    "crossed_out" => Modifier::CROSSED_OUT,
                    "hidden" => Modifier::HIDDEN,
                    other => return Err(ShotError::Frame(format!("unknown style {other}"))),
                });
            }
            buffer.set_string(run.x, y as u16, &run.text, style);
        }
    }
    Ok(buffer)
}

pub fn rasterize(buffer: &Buffer, theme: Theme) -> Result<Raster, ShotError> {
    let fonts = Fonts::shared()?;
    let width = u32::from(buffer.area.width) * CELL_WIDTH;
    let height = u32::from(buffer.area.height) * CELL_HEIGHT;
    let mut raster = Raster {
        width,
        height,
        pixels: vec![0; width as usize * height as usize * 3],
    };

    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let cell = &buffer[(x, y)];
            paint_cell(&mut raster, fonts, cell, theme, x, y);
        }
    }
    Ok(raster)
}

fn paint_cell(raster: &mut Raster, fonts: &Fonts, cell: &Cell, theme: Theme, x: u16, y: u16) {
    let mut fg = color_rgb(cell.fg, theme, false);
    let mut bg = color_rgb(cell.bg, theme, true);
    if cell.modifier.contains(Modifier::REVERSED) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.modifier.contains(Modifier::DIM) {
        fg = blend(bg, fg, 0.55);
    }
    if cell.modifier.contains(Modifier::HIDDEN) {
        fg = bg;
    }

    let left = u32::from(x) * CELL_WIDTH;
    let top = u32::from(y) * CELL_HEIGHT;
    fill_rect(raster, left, top, CELL_WIDTH, CELL_HEIGHT, bg);
    // Lines the terminal draws across the cell, spaces included, so a
    // link's underline runs unbroken between its words.
    if fg != bg {
        if cell.modifier.contains(Modifier::UNDERLINED) {
            let at = (top as i32 + BASELINE + 2).min((top + CELL_HEIGHT) as i32 - 1) as u32;
            fill_rect(raster, left, at, CELL_WIDTH, 1, fg);
        }
        if cell.modifier.contains(Modifier::CROSSED_OUT) {
            fill_rect(raster, left, top + CELL_HEIGHT / 2 + 1, CELL_WIDTH, 1, fg);
        }
    }

    let symbol = cell.symbol();
    if symbol.trim().is_empty() || fg == bg {
        return;
    }
    // Terminals draw block elements themselves, edge to edge, rather than
    // from the font, so rows of them join without seams. Drawn the same way
    // here, a picture shows what the terminal shows.
    let half = CELL_HEIGHT / 2;
    match symbol {
        "█" => return fill_rect(raster, left, top, CELL_WIDTH, CELL_HEIGHT, fg),
        "▀" => return fill_rect(raster, left, top, CELL_WIDTH, half, fg),
        "▄" => return fill_rect(raster, left, top + half, CELL_WIDTH, CELL_HEIGHT - half, fg),
        _ => {}
    }

    let chars = symbol.chars().filter(|character| !character.is_control());
    let mut pen_x = left as i32;
    for character in chars {
        let font = fonts.for_character(cell, character);
        let glyph = font.lookup_glyph_index(character);
        if glyph == 0 && character != '\0' {
            draw_missing_glyph(raster, left, top, fg);
            break;
        }
        let (metrics, bitmap) = font.rasterize(character, FONT_SIZE);
        let glyph_left = pen_x + metrics.xmin;
        let glyph_top = top as i32 + BASELINE - metrics.height as i32 - metrics.ymin;
        for glyph_y in 0..metrics.height {
            for glyph_x in 0..metrics.width {
                let alpha = bitmap[glyph_y * metrics.width + glyph_x];
                if alpha == 0 {
                    continue;
                }
                let pixel_x = glyph_left + glyph_x as i32;
                let pixel_y = glyph_top + glyph_y as i32;
                if pixel_x < left as i32
                    || pixel_x >= (left + CELL_WIDTH) as i32
                    || pixel_y < top as i32
                    || pixel_y >= (top + CELL_HEIGHT) as i32
                {
                    continue;
                }
                blend_pixel(raster, pixel_x as u32, pixel_y as u32, fg, alpha);
            }
        }
        pen_x += metrics.advance_width.round() as i32;
        if pen_x >= (left + CELL_WIDTH) as i32 {
            break;
        }
    }
}

fn draw_missing_glyph(raster: &mut Raster, left: u32, top: u32, color: [u8; 3]) {
    for x in left + 2..left + CELL_WIDTH.saturating_sub(2) {
        put_pixel(raster, x, top + 3, color);
        put_pixel(raster, x, top + CELL_HEIGHT - 4, color);
    }
    for y in top + 3..top + CELL_HEIGHT.saturating_sub(3) {
        put_pixel(raster, left + 2, y, color);
        put_pixel(raster, left + CELL_WIDTH - 3, y, color);
    }
}

fn color_rgb(color: Color, theme: Theme, background: bool) -> [u8; 3] {
    match color {
        Color::Reset => {
            let token = if background {
                theme.tokens.background
            } else {
                theme.tokens.text
            };
            resolved_rgb(token.resolve(theme.mode), token.rgb)
        }
        Color::Black => [0, 0, 0],
        Color::Red => [205, 49, 49],
        Color::Green => [13, 188, 121],
        Color::Yellow => [229, 229, 16],
        Color::Blue => [36, 114, 200],
        Color::Magenta => [188, 63, 188],
        Color::Cyan => [17, 168, 205],
        Color::Gray => [204, 204, 204],
        Color::DarkGray => [102, 102, 102],
        Color::LightRed => [241, 76, 76],
        Color::LightGreen => [35, 209, 139],
        Color::LightYellow => [245, 245, 67],
        Color::LightBlue => [59, 142, 234],
        Color::LightMagenta => [214, 112, 214],
        Color::LightCyan => [41, 184, 219],
        Color::White => [242, 242, 242],
        Color::Rgb(red, green, blue) => [red, green, blue],
        Color::Indexed(index) => indexed_rgb(index),
    }
}

fn resolved_rgb(color: Color, fallback: (u8, u8, u8)) -> [u8; 3] {
    match color {
        Color::Rgb(red, green, blue) => [red, green, blue],
        Color::Reset => [fallback.0, fallback.1, fallback.2],
        other => color_rgb_for_named(other).unwrap_or([fallback.0, fallback.1, fallback.2]),
    }
}

fn color_rgb_for_named(color: Color) -> Option<[u8; 3]> {
    Some(match color {
        Color::Black => [0, 0, 0],
        Color::Red => [205, 49, 49],
        Color::Green => [13, 188, 121],
        Color::Yellow => [229, 229, 16],
        Color::Blue => [36, 114, 200],
        Color::Magenta => [188, 63, 188],
        Color::Cyan => [17, 168, 205],
        Color::Gray => [204, 204, 204],
        Color::DarkGray => [102, 102, 102],
        Color::LightRed => [241, 76, 76],
        Color::LightGreen => [35, 209, 139],
        Color::LightYellow => [245, 245, 67],
        Color::LightBlue => [59, 142, 234],
        Color::LightMagenta => [214, 112, 214],
        Color::LightCyan => [41, 184, 219],
        Color::White => [242, 242, 242],
        Color::Indexed(index) => indexed_rgb(index),
        Color::Rgb(red, green, blue) => [red, green, blue],
        Color::Reset => return None,
    })
}

fn indexed_rgb(index: u8) -> [u8; 3] {
    const ANSI: [[u8; 3]; 16] = [
        [0, 0, 0],
        [205, 49, 49],
        [13, 188, 121],
        [229, 229, 16],
        [36, 114, 200],
        [188, 63, 188],
        [17, 168, 205],
        [204, 204, 204],
        [102, 102, 102],
        [241, 76, 76],
        [35, 209, 139],
        [245, 245, 67],
        [59, 142, 234],
        [214, 112, 214],
        [41, 184, 219],
        [242, 242, 242],
    ];
    if index < 16 {
        return ANSI[index as usize];
    }
    if index < 232 {
        let value = index - 16;
        let channel = |part: u8| if part == 0 { 0 } else { 55 + part * 40 };
        return [
            channel(value / 36),
            channel((value % 36) / 6),
            channel(value % 6),
        ];
    }
    let gray = 8 + (index - 232) * 10;
    [gray, gray, gray]
}

fn blend(background: [u8; 3], foreground: [u8; 3], amount: f32) -> [u8; 3] {
    std::array::from_fn(|index| {
        (f32::from(background[index]) * (1.0 - amount) + f32::from(foreground[index]) * amount)
            .round() as u8
    })
}

fn fill_rect(raster: &mut Raster, x: u32, y: u32, width: u32, height: u32, color: [u8; 3]) {
    for pixel_y in y..y + height {
        for pixel_x in x..x + width {
            put_pixel(raster, pixel_x, pixel_y, color);
        }
    }
}

fn blend_pixel(raster: &mut Raster, x: u32, y: u32, foreground: [u8; 3], alpha: u8) {
    let offset = (y as usize * raster.width as usize + x as usize) * 3;
    let amount = f32::from(alpha) / 255.0;
    for (channel, foreground) in foreground.into_iter().enumerate() {
        raster.pixels[offset + channel] = (f32::from(raster.pixels[offset + channel])
            * (1.0 - amount)
            + f32::from(foreground) * amount)
            .round() as u8;
    }
}

fn put_pixel(raster: &mut Raster, x: u32, y: u32, color: [u8; 3]) {
    let offset = (y as usize * raster.width as usize + x as usize) * 3;
    raster.pixels[offset..offset + 3].copy_from_slice(&color);
}

/// Write a raster as a deterministic RGB PNG.
pub fn write_png(raster: &Raster, out: &Path) -> Result<(), ShotError> {
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(out)?;
    let writer = BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, raster.width, raster.height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::High);
    let mut png = encoder
        .write_header()
        .map_err(|error| ShotError::Encode(error.to_string()))?;
    png.write_image_data(&raster.pixels)
        .map_err(|error| ShotError::Encode(error.to_string()))?;
    Ok(())
}

/// Record a receipt after a complete render-set invocation.
pub fn append_set(dir: &Path, name: &str, files: Vec<String>) -> Result<(), ShotError> {
    let mut manifest = read_manifest_or_default(&dir.join("manifest.json"))?;
    let set = ManifestSet {
        name: name.to_string(),
        files,
    };
    if let Some(existing) = manifest
        .sets
        .iter_mut()
        .find(|existing| existing.name == name)
    {
        *existing = set;
    } else {
        manifest.sets.push(set);
    }
    write_manifest(dir, &manifest)
}

fn append_entry(dir: &Path, entry: ManifestEntry) -> Result<(), ShotError> {
    let mut manifest = read_manifest_or_default(&dir.join("manifest.json"))?;
    if let Some(existing) = manifest
        .entries
        .iter_mut()
        .find(|existing| existing.file == entry.file)
    {
        *existing = entry;
    } else {
        manifest.entries.push(entry);
    }
    write_manifest(dir, &manifest)
}

fn read_manifest_or_default(path: &Path) -> Result<Manifest, ShotError> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| ShotError::Verify(format!("{}: {error}", path.display()))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Manifest::default()),
        Err(error) => Err(error.into()),
    }
}

fn write_manifest(dir: &Path, manifest: &Manifest) -> Result<(), ShotError> {
    fs::create_dir_all(dir)?;
    let path = dir.join("manifest.json");
    let temporary = dir.join("manifest.json.tmp");
    let mut bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| ShotError::Verify(format!("manifest serialization: {error}")))?;
    bytes.push(b'\n');
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)?;
    Ok(())
}

/// Verify every manifest and PNG below `dir`.
pub fn verify(dir: &Path) -> Result<Manifest, ShotError> {
    let mut manifests = Vec::new();
    find_manifests(dir, &mut manifests)?;
    if manifests.is_empty() {
        return Err(ShotError::Verify(format!(
            "no manifest.json found below {}",
            dir.display()
        )));
    }

    let mut combined = Manifest::default();
    for path in manifests {
        let bytes = fs::read(&path)?;
        let manifest: Manifest = serde_json::from_slice(&bytes)
            .map_err(|error| ShotError::Verify(format!("{}: {error}", path.display())))?;
        let base = path.parent().expect("manifest has a parent");
        for entry in &manifest.entries {
            verify_entry(base, entry)?;
        }
        for set in &manifest.sets {
            for file in &set.files {
                if !manifest.entries.iter().any(|entry| &entry.file == file) {
                    return Err(ShotError::Verify(format!(
                        "set `{}` is missing manifest entry `{file}`",
                        set.name
                    )));
                }
                if !base.join(file).is_file() {
                    return Err(ShotError::Verify(format!(
                        "set `{}` is missing file `{file}`",
                        set.name
                    )));
                }
            }
        }
        combined.entries.extend(manifest.entries);
        combined.sets.extend(manifest.sets);
    }
    Ok(combined)
}

fn find_manifests(path: &Path, manifests: &mut Vec<PathBuf>) -> Result<(), ShotError> {
    if path.is_file() {
        if path.file_name().is_some_and(|name| name == "manifest.json") {
            manifests.push(path.to_path_buf());
        }
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            find_manifests(&entry.path(), manifests)?;
        } else if entry.file_name() == "manifest.json" {
            manifests.push(entry.path());
        }
    }
    manifests.sort();
    Ok(())
}

fn verify_entry(base: &Path, entry: &ManifestEntry) -> Result<(), ShotError> {
    let expected_pixels = [
        u32::from(entry.viewport[0]) * CELL_WIDTH,
        u32::from(entry.viewport[1]) * CELL_HEIGHT,
    ];
    if entry.pixels != expected_pixels {
        return Err(ShotError::Verify(format!(
            "{} records pixel size {}x{} instead of {}x{}",
            entry.file, entry.pixels[0], entry.pixels[1], expected_pixels[0], expected_pixels[1]
        )));
    }
    let path = base.join(&entry.file);
    let bytes = fs::read(&path)
        .map_err(|error| ShotError::Verify(format!("{}: {error}", path.display())))?;
    let digest = hex_digest(&bytes);
    if digest != entry.sha256 {
        return Err(ShotError::Verify(format!(
            "{} has sha256 {digest}, expected {}",
            path.display(),
            entry.sha256
        )));
    }

    let decoder = png::Decoder::new(BufReader::new(File::open(&path)?));
    let mut reader = decoder
        .read_info()
        .map_err(|error| ShotError::Verify(format!("{}: {error}", path.display())))?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| ShotError::Verify(format!("{}: too large to decode", path.display())))?;
    let mut pixels = vec![0; size];
    let info = reader
        .next_frame(&mut pixels)
        .map_err(|error| ShotError::Verify(format!("{}: {error}", path.display())))?;
    if [info.width, info.height] != expected_pixels {
        return Err(ShotError::Verify(format!(
            "{} is {}x{}, expected {}x{}",
            path.display(),
            info.width,
            info.height,
            expected_pixels[0],
            expected_pixels[1]
        )));
    }
    Ok(())
}

fn color_mode_name(mode: ColorMode) -> &'static str {
    match mode {
        ColorMode::TrueColor => "truecolor",
        ColorMode::Ansi => "ansi",
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut result = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(result, "{byte:02x}").expect("writing to a String cannot fail");
    }
    result
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;

    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Modifier};
    use tempfile::tempdir;
    use tui::vocabulary::components;
    use tui::{ColorMode, Theme};

    use super::{
        CELL_HEIGHT, CELL_WIDTH, Fonts, ShotError, rasterize, render_to_path, render_vocabulary,
        report_frame, verify,
    };

    /// Every non-ASCII glyph the vocabulary draws has a vendored face in
    /// every style, so no capture shows a missing-glyph box for chrome.
    #[test]
    fn every_glyph_the_vocabulary_draws_has_a_vendored_face() {
        let fonts = Fonts::shared().unwrap();
        let mut glyphs = BTreeSet::new();
        for component in components(Theme::default()) {
            for cell in component.buffer.content() {
                glyphs.extend(cell.symbol().chars().filter(|c| !c.is_ascii()));
            }
        }
        assert!(!glyphs.is_empty());
        for modifier in [
            Modifier::empty(),
            Modifier::BOLD,
            Modifier::ITALIC,
            Modifier::BOLD | Modifier::ITALIC,
        ] {
            for character in &glyphs {
                let mut cell = ratatui::buffer::Cell::default();
                cell.modifier = modifier;
                let font = fonts.for_character(&cell, *character);
                assert_ne!(
                    font.lookup_glyph_index(*character),
                    0,
                    "U+{:04X} `{character}` is missing for {modifier:?}",
                    *character as u32
                );
            }
        }
    }

    #[test]
    fn the_vocabulary_renders_one_verified_png_per_component() {
        let directory = tempdir().unwrap();
        let theme = Theme::dark(ColorMode::TrueColor);
        let entries = render_vocabulary(directory.path(), theme, "dark").unwrap();
        assert_eq!(entries.len(), components(theme).len());
        let manifest = verify(directory.path()).unwrap();
        assert_eq!(manifest.entries.len(), entries.len());
        assert_eq!(manifest.sets.len(), 1);
        for entry in &entries {
            let decoder = png::Decoder::new(std::io::BufReader::new(
                fs::File::open(directory.path().join(&entry.file)).unwrap(),
            ));
            let reader = decoder.read_info().unwrap();
            assert_eq!(
                [reader.info().width, reader.info().height],
                [
                    u32::from(entry.viewport[0]) * CELL_WIDTH,
                    u32::from(entry.viewport[1]) * CELL_HEIGHT
                ]
            );
        }
    }

    #[test]
    fn two_renders_are_byte_identical() {
        let first = tempdir().unwrap();
        let second = tempdir().unwrap();
        let theme = Theme::light(ColorMode::TrueColor);
        let a = render_vocabulary(first.path(), theme, "light").unwrap();
        let b = render_vocabulary(second.path(), theme, "light").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn rasterizer_honors_cell_colors_and_modifiers() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 5, 1));
        for x in 0..4 {
            let cell = &mut buffer[(x, 0)];
            cell.set_symbol("A");
            cell.fg = Color::White;
            cell.bg = Color::Black;
        }
        buffer[(1, 0)].modifier = Modifier::BOLD;
        buffer[(2, 0)].modifier = Modifier::ITALIC;
        buffer[(3, 0)].modifier = Modifier::DIM;
        buffer[(4, 0)].bg = Color::Red;

        let raster = rasterize(&buffer, Theme::dark(ColorMode::TrueColor)).unwrap();
        let region = |cell: u32| {
            let mut bytes = Vec::new();
            for y in 0..CELL_HEIGHT {
                let start = (y * raster.width + cell * CELL_WIDTH) as usize * 3;
                bytes.extend_from_slice(&raster.pixels[start..start + CELL_WIDTH as usize * 3]);
            }
            bytes
        };

        assert_ne!(region(0), region(1), "bold uses the bold face");
        assert_ne!(region(0), region(2), "italic uses the italic face");
        assert_ne!(
            region(0),
            region(3),
            "dim blends foreground toward background"
        );
        let brightness = |bytes: Vec<u8>| bytes.into_iter().map(u64::from).sum::<u64>();
        assert!(brightness(region(0)) > brightness(region(3)));
        assert_eq!(&region(4)[..3], &[205, 49, 49]);
    }

    #[test]
    fn verify_rejects_a_truncated_png() {
        let directory = tempdir().unwrap();
        let output = directory.path().join("row.png");
        let theme = Theme::dark(ColorMode::TrueColor);
        let component = components(theme).remove(0);
        render_to_path(component.name, &component.buffer, theme, "dark", &output).unwrap();
        let mut bytes = fs::read(&output).unwrap();
        bytes.truncate(bytes.len() / 2);
        fs::write(&output, bytes).unwrap();
        let error = verify(directory.path()).unwrap_err();
        assert!(error.to_string().contains("verification failed"));
    }

    #[test]
    fn a_reports_frame_is_rebuilt_cell_for_cell() {
        let frame = r##"{"width": 6, "height": 2, "default": {"fg": "#eeeeee", "bg": "#111111"},
            "rows": [[{"x": 0, "text": "ab", "fg": "#ff0000", "bg": "default", "styles": ["bold"]},
                      {"x": 2, "text": "c", "fg": "ansi:4", "bg": "ansi:0"}], []]}"##;
        let buffer = report_frame(frame).unwrap();
        assert_eq!(buffer[(0, 0)].symbol(), "a");
        assert_eq!(buffer[(0, 0)].fg, Color::Rgb(255, 0, 0));
        assert_eq!(buffer[(0, 0)].bg, Color::Rgb(17, 17, 17));
        assert!(buffer[(1, 0)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(2, 0)].fg, Color::Indexed(4));
        assert_eq!(buffer[(5, 1)].fg, Color::Rgb(238, 238, 238));
        assert!(matches!(
            report_frame(r#"{"width": 1}"#),
            Err(ShotError::Frame(_))
        ));
    }
}
