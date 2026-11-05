//! QR codes drawn in a terminal: two modules to a cell, one above the
//! other, so a module is as wide as it is tall in the usual cell, and in
//! the terminal's own two colours, so the code reads on any palette.

use qrcode::{EcLevel, QrCode};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// The light margin a scanner needs around a code, in modules across. A
/// camera finds a code with a narrower margin than the standard's four, and
/// every column counts on a terminal.
const QUIET_ACROSS: usize = 2;
/// The margin above and below, in cells: two modules.
const QUIET_ROWS: usize = 1;

/// A QR code's modules, dark or not, row by row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Code {
    width: usize,
    dark: Vec<bool>,
}

impl Code {
    /// The code for `text` at the lowest error correction, which keeps it
    /// smallest: a screen is not a sticker that wears. None when it is too
    /// long for any QR code.
    pub fn encode(text: &str) -> Option<Code> {
        let code = QrCode::with_error_correction_level(text.as_bytes(), EcLevel::L).ok()?;
        Some(Code {
            width: code.width(),
            dark: code
                .to_colors()
                .into_iter()
                .map(|color| color == qrcode::Color::Dark)
                .collect(),
        })
    }

    /// Columns and rows the code takes with its margin.
    pub fn cells(&self) -> (usize, usize) {
        let side = self.width + 2 * QUIET_ACROSS;
        (side, self.width.div_ceil(2) + 2 * QUIET_ROWS)
    }

    fn dark(&self, x: isize, y: isize) -> bool {
        let width = self.width as isize;
        (0..width).contains(&x) && (0..width).contains(&y) && self.dark[(y * width + x) as usize]
    }

    /// The code as lines of block characters in `ink` on the ground. A
    /// scanner wants dark modules on light: on a dark ground the light
    /// modules and the margin are inked, on a light ground the dark ones.
    pub fn lines(&self, dark_ground: bool, ink: Style) -> Vec<Line<'static>> {
        let (columns, rows) = self.cells();
        let inked = |x: isize, y: isize| self.dark(x, y) != dark_ground;
        (0..rows)
            .map(|row| {
                let top = (row as isize - QUIET_ROWS as isize) * 2;
                let text: String = (0..columns)
                    .map(|column| {
                        let x = column as isize - QUIET_ACROSS as isize;
                        match (inked(x, top), inked(x, top + 1)) {
                            (true, true) => '█',
                            (true, false) => '▀',
                            (false, true) => '▄',
                            (false, false) => ' ',
                        }
                    })
                    .collect();
                Line::from(Span::styled(text, ink))
            })
            .collect()
    }
}

/// `link` as `amux pair --qr` prints it, in the terminal's text colour on
/// its ground, with the standard margin.
pub fn printed(link: &str) -> Option<String> {
    let code = QrCode::new(link.as_bytes()).ok()?;
    Some(
        code.render::<qrcode::render::unicode::Dense1x2>()
            .quiet_zone(true)
            .build(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_is_two_modules_to_a_cell_with_its_margin() {
        let code = Code::encode("https://amux.sh/p").unwrap();
        // Version 1: 21 modules.
        assert_eq!(code.cells(), (25, 13));
        let lines = code.lines(true, Style::default());
        assert_eq!(lines.len(), 13);
        // On a dark ground the margin is inked, light as the page.
        let first: String = lines[0].spans[0].content.to_string();
        assert!(first.chars().all(|c| c == '█'), "{first:?}");
        // On a light ground it is the ground itself.
        let light = code.lines(false, Style::default());
        assert!(light[0].spans[0].content.trim().is_empty());
    }
}
