//! Image thumbnails in the chat's feed, on terminals that can draw images.
//!
//! The feed leaves blank lines for a thumbnail, each tagged in its hits with
//! the image and how far down it is, so a thumbnail scrolls and clips with
//! the lines around it. The drawing goes over those lines afterwards; until
//! the image is here the same box says what is happening.
//!
//! The box has a fixed size. An attachment's reference does not carry its
//! dimensions, and a box that took the image's shape when it landed would
//! move everything below it under the reader.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use image::DynamicImage;
use ratatui::Frame as Paint;
use ratatui::layout::{Rect, Size};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui_image::Resize;
use ratatui_image::picker::Picker;
use ratatui_image::sliced::{SignedPosition, SlicedImage, SlicedProtocol};
use ui_state::BlobStatus;
use wire::BlobRef;

use super::feed::{FeedHit, LineHits};
use crate::text;
use crate::theme::Theme;

/// A thumbnail's height, in lines.
pub const ROWS: u16 = 10;
/// A thumbnail's width at most, in columns.
pub const COLUMNS: u16 = 40;
/// The narrowest feed that leaves room for one, at the deepest column one
/// starts in. Some protocols draw nothing at all when an image is wider than
/// its area, so a narrower feed keeps the chip.
pub const LEAST_FEED_WIDTH: usize = COLUMNS as usize + 8;

enum Thumb {
    Decoding,
    Ready {
        image: SlicedProtocol,
        /// A protocol sends the image with the first drawing that names it,
        /// so that drawing must reach the screen.
        sent: bool,
    },
    /// The bytes are here and are not an image we can read.
    Unreadable,
}

/// The chat's thumbnails, by the image's hash.
#[derive(Default)]
pub struct Thumbs {
    picker: Option<Picker>,
    thumbs: HashMap<Vec<u8>, Thumb>,
    /// Images on screen whose thumbnails are not made yet.
    wanted: HashSet<Vec<u8>>,
}

impl std::fmt::Debug for Thumbs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Thumbs")
            .field("enabled", &self.enabled())
            .field("thumbs", &self.thumbs.len())
            .finish()
    }
}

/// Where a thumbnail's lines are in the feed.
struct Placed {
    blob: BlobRef,
    column: u16,
    /// The feed line its first row falls on; above the feed when negative.
    top: i16,
}

impl Thumbs {
    pub fn new(picker: Option<Picker>) -> Self {
        Self {
            picker,
            ..Self::default()
        }
    }

    /// This terminal can draw images.
    pub fn enabled(&self) -> bool {
        self.picker.is_some()
    }

    /// The terminal was left and has come back: what it held is gone.
    pub fn forget(&mut self) {
        self.thumbs.clear();
        self.wanted.clear();
    }

    /// The images to make thumbnails for, each asked once.
    pub fn take_wanted(&mut self) -> Vec<Vec<u8>> {
        self.wanted.drain().collect()
    }

    /// Its bytes are being decoded.
    pub fn decoding(&mut self, hash: Vec<u8>) {
        self.thumbs.insert(hash, Thumb::Decoding);
    }

    /// The largest an image needs to be to fill the box sharply.
    pub fn pixels(&self) -> Option<(u32, u32)> {
        let font = self.picker.as_ref()?.font_size();
        Some((
            u32::from(COLUMNS) * u32::from(font.width),
            u32::from(ROWS) * u32::from(font.height),
        ))
    }

    /// A decoded image arrived, or None when its bytes could not be read.
    pub fn decoded(&mut self, hash: Vec<u8>, image: Option<DynamicImage>) {
        let Some(picker) = &self.picker else {
            return;
        };
        let made = image.and_then(|image| {
            SlicedProtocol::new_with_resize(
                picker,
                image,
                Size::new(COLUMNS, ROWS),
                Resize::Fit(None),
            )
            .ok()
        });
        let thumb = match made {
            Some(image) => Thumb::Ready { image, sent: false },
            None => Thumb::Unreadable,
        };
        self.thumbs.insert(hash, thumb);
    }

    /// Draws every thumbnail on the feed lines in `visible`. `covered`
    /// says something will be drawn over the feed this frame.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        paint: &mut Paint<'_>,
        feed: Rect,
        hits: &[LineHits],
        visible: Range<usize>,
        blob_status: impl Fn(&[u8]) -> BlobStatus,
        covered: bool,
        theme: Theme,
    ) {
        let visible = visible.start..visible.end.min(hits.len());
        let area = Rect {
            y: feed.y + visible.start as u16,
            height: visible.len() as u16,
            ..feed
        };
        for placed in placements(hits, visible.clone()) {
            let top = placed.top - visible.start as i16;
            // Cut at both ends only a feed shorter than the picture can do.
            // `SlicedImage` keeps too many rows then, for iTerm2, and would
            // draw past the feed over the composer.
            let cut_both_ends = top < 0 && top + ROWS as i16 > area.height as i16;
            match self.thumbs.get_mut(&placed.blob.hash) {
                Some(Thumb::Ready { .. }) if cut_both_ends => {
                    placeholder(paint, area, &placed, top, "", theme);
                }
                Some(Thumb::Ready { image, sent }) => {
                    if *sent || !covered {
                        *sent = true;
                        let position = SignedPosition::from((placed.column as i16, top));
                        paint.render_widget(SlicedImage::new(image, position), area);
                    }
                }
                Some(Thumb::Unreadable) => {
                    placeholder(paint, area, &placed, top, "No preview", theme);
                }
                Some(Thumb::Decoding) => {
                    placeholder(paint, area, &placed, top, "Loading…", theme);
                }
                None => {
                    let words = match blob_status(&placed.blob.hash) {
                        BlobStatus::Failed(_) => "Couldn't load · click to retry",
                        _ => "Loading…",
                    };
                    placeholder(paint, area, &placed, top, words, theme);
                    self.wanted.insert(placed.blob.hash.clone());
                }
            }
        }
    }
}

/// Each thumbnail with a line in `visible`, once.
fn placements(hits: &[LineHits], visible: Range<usize>) -> Vec<Placed> {
    let mut placed: Vec<Placed> = Vec::new();
    for line in visible {
        for (span, hit) in &hits[line] {
            if let (Some((from, _)), FeedHit::Thumbnail { blob, row }) = (span, hit) {
                let top = line as i16 - *row as i16;
                if !placed.iter().any(|seen| seen.top == top) {
                    placed.push(Placed {
                        blob: blob.clone(),
                        column: *from as u16,
                        top,
                    });
                }
            }
        }
    }
    placed
}

/// The box before its picture: a raised ground, the words in its middle.
fn placeholder(
    paint: &mut Paint<'_>,
    area: Rect,
    placed: &Placed,
    top: i16,
    words: &str,
    theme: Theme,
) {
    let left = area.x + placed.column;
    let width = COLUMNS.min(area.width.saturating_sub(placed.column));
    let from = top.max(0) as u16;
    let to = (top + ROWS as i16).clamp(0, area.height as i16) as u16;
    if to <= from || width == 0 {
        return;
    }
    paint.buffer_mut().set_style(
        Rect::new(left, area.y + from, width, to - from),
        theme.chip_raised(),
    );
    let middle = top + (ROWS / 2) as i16;
    if (from as i16..to as i16).contains(&middle) {
        let words = text::ellipsize(words, width as usize);
        let inset = (width as usize).saturating_sub(text::str_width(&words)) / 2;
        let line = Line::from(vec![
            Span::raw(" ".repeat(inset)),
            Span::styled(words, theme.faint()),
        ]);
        paint.render_widget(
            Paragraph::new(line).style(theme.chip_raised()),
            Rect::new(left, area.y + middle as u16, width, 1),
        );
    }
}

/// Reads an image and shrinks it to fit `pixels`. Slow: not for the draw
/// loop.
pub fn decode(bytes: &[u8], pixels: (u32, u32)) -> Option<DynamicImage> {
    let image = image::load_from_memory(bytes).ok()?;
    Some(image.thumbnail(pixels.0, pixels.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(tag: u8) -> BlobRef {
        BlobRef {
            hash: vec![tag],
            ..BlobRef::default()
        }
    }

    fn lines(blob: &BlobRef) -> Vec<LineHits> {
        (0..ROWS)
            .map(|row| {
                vec![(
                    Some((4, 44)),
                    FeedHit::Thumbnail {
                        blob: blob.clone(),
                        row,
                    },
                )]
            })
            .collect()
    }

    #[test]
    fn a_thumbnail_is_placed_once_however_many_lines_show() {
        let placed = placements(&lines(&blob(1)), 0..10);
        assert_eq!(placed.len(), 1);
        assert_eq!((placed[0].column, placed[0].top), (4, 0));
    }

    #[test]
    fn a_thumbnail_scrolled_partly_away_keeps_its_true_top() {
        let hits = lines(&blob(1));
        let placed = placements(&hits[4..], 0..6);
        assert_eq!(placed[0].top, -4);
    }

    #[test]
    fn two_copies_of_one_image_are_two_thumbnails() {
        let image = blob(1);
        let mut hits = lines(&image);
        hits.extend(lines(&image));
        let mut second = placements(&hits, 0..20);
        assert_eq!(second.len(), 2);
        assert_eq!(second.pop().unwrap().top, 10);
    }

    #[test]
    fn an_unreadable_image_is_reported_as_none() {
        assert!(decode(b"not an image", (10, 10)).is_none());
    }
}
