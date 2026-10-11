//! Golden captures: what the app draws, compared with what it drew last time.
//!
//! `apps/apple/Goldens/manifest.json` names every golden the phone is held
//! to, in two parts. Components are photographed in-process from authored
//! view data by the component snapshot suite, one sentence each saying what
//! the picture shows. Screens are whole displays the golden driver
//! (`scripts/ios-goldens.py`) reaches on a served network through the app's
//! door, each an image per appearance plus the door's element geometry. This
//! module is the pixel comparison both the golden driver and the journeys
//! call, and the check that the manifest and the baselines agree.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// A region of a simulator's display that the system draws over every app,
/// which no capture compares.
///
/// A golden is a photograph of the display, so it holds whatever SpringBoard
/// puts over the app as well as the app. The status bar is pinned, but the
/// home indicator cannot be: SpringBoard draws it when an app launches and
/// withdraws it once backboardd's attention timer says nobody is touching the
/// screen, and on a GitHub runner with both pinned devices booted that timer's
/// event is delivered to a stale client, so the bar never withdraws there and
/// always does on a developer's Mac. The bar is not the app's drawing, so the
/// pixels under it are not the app's golden; they are painted over in the
/// difference image so nobody mistakes them for compared ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemChrome {
    /// What the system draws there, for whoever reads the manifest.
    pub what: String,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl SystemChrome {
    fn covers(&self, x: u32, y: u32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

/// What the manifest knows about one pinned simulator.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoldenSimulator {
    #[serde(default)]
    pub system_chrome: Vec<SystemChrome>,
}

/// A picture with a sentence saying what it shows, for whoever reviews it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenComponent {
    pub id: String,
    pub shows: String,
}

/// One moment of a screen that is photographed more than once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenFrame {
    pub name: String,
    pub shows: String,
}

/// A whole screen reached through the door. A screen with frames has a
/// golden per frame, named `<id>-<frame>`; one without has one named `<id>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenScreen {
    pub id: String,
    pub shows: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<GoldenFrame>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenManifest {
    /// The simulator every screen is photographed on, with the chrome it
    /// draws over the app.
    pub simulators: BTreeMap<String, GoldenSimulator>,
    /// The served topology the screens are reached on.
    pub topology: String,
    pub components: Vec<GoldenComponent>,
    pub screens: Vec<GoldenScreen>,
}

impl GoldenManifest {
    pub fn read(path: &Path) -> Result<Self, GoldenError> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| GoldenError::Io(format!("{}: {error}", path.display())))?;
        let manifest: Self = serde_json::from_str(&text)
            .map_err(|error| GoldenError::Io(format!("{}: {error}", path.display())))?;
        let mut seen = BTreeSet::new();
        let pictures = manifest
            .components
            .iter()
            .map(|component| (&component.id, &component.shows))
            .chain(
                manifest
                    .screens
                    .iter()
                    .map(|screen| (&screen.id, &screen.shows)),
            )
            .chain(manifest.screens.iter().flat_map(|screen| {
                screen
                    .frames
                    .iter()
                    .map(|frame| (&frame.name, &frame.shows))
            }));
        for (id, shows) in pictures {
            if shows.trim().is_empty() {
                return Err(GoldenError::Io(format!(
                    "{}: {id} does not say what it shows",
                    path.display()
                )));
            }
        }
        for id in manifest
            .components
            .iter()
            .map(|component| &component.id)
            .chain(manifest.screens.iter().map(|screen| &screen.id))
        {
            if !seen.insert(id) {
                return Err(GoldenError::Io(format!(
                    "{}: {id} is named twice",
                    path.display()
                )));
            }
        }
        Ok(manifest)
    }
}

/// What a comparison found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoldenVerdict {
    Same,
    /// A match that spent some of the allowance for stray pixels: how many
    /// moved further than rounding, and the furthest any channel moved.
    Close {
        pixels: u64,
        largest: u8,
    },
    Different {
        pixels: u64,
        largest: u8,
        first: (u32, u32),
    },
    SizeMismatch {
        expected: (u32, u32),
        actual: (u32, u32),
    },
    /// Nothing to compare against: the screen has never been locked.
    MissingBaseline,
    /// The capture was never written.
    CaptureFailed(String),
}

impl GoldenVerdict {
    pub fn passed(&self) -> bool {
        matches!(self, Self::Same | Self::Close { .. })
    }
}

impl fmt::Display for GoldenVerdict {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Same => write!(out, "same"),
            Self::Close { pixels, largest } => write!(
                out,
                "same but for {pixels} pixels off by up to {largest} levels"
            ),
            Self::Different {
                pixels,
                largest,
                first,
            } => write!(
                out,
                "{pixels} pixels differ by up to {largest} levels, first at {},{}",
                first.0, first.1
            ),
            Self::SizeMismatch { expected, actual } => write!(
                out,
                "the capture is {}x{} and the baseline is {}x{}",
                actual.0, actual.1, expected.0, expected.1
            ),
            Self::MissingBaseline => write!(out, "no baseline"),
            Self::CaptureFailed(why) => write!(out, "{why}"),
        }
    }
}

#[derive(Debug)]
pub enum GoldenError {
    Io(String),
    Png(String),
}

impl fmt::Display for GoldenError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) | Self::Png(message) => out.write_str(message),
        }
    }
}

impl std::error::Error for GoldenError {}

impl From<std::io::Error> for GoldenError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

struct Image {
    width: u32,
    height: u32,
    /// RGBA, row-major.
    pixels: Vec<u8>,
}

fn read_png(path: &Path) -> Result<Image, GoldenError> {
    let file = std::fs::File::open(path)
        .map_err(|error| GoldenError::Io(format!("{}: {error}", path.display())))?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    // The simulator writes wide-gamut captures at sixteen bits a channel, and
    // a baseline written by an earlier run may be eight. Both are read as
    // eight so a comparison is always like for like; nothing a golden is
    // about lives in the low byte.
    decoder.set_transformations(png::Transformations::STRIP_16 | png::Transformations::EXPAND);
    let mut reading = decoder
        .read_info()
        .map_err(|error| GoldenError::Png(format!("{}: {error}", path.display())))?;
    let mut buffer = vec![0; reading.output_buffer_size()];
    let info = reading
        .next_frame(&mut buffer)
        .map_err(|error| GoldenError::Png(format!("{}: {error}", path.display())))?;
    buffer.truncate(info.buffer_size());
    let channels = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        other => {
            return Err(GoldenError::Png(format!(
                "{}: captures are RGB or RGBA, not {other:?}",
                path.display()
            )));
        }
    };
    let pixels = if channels == 4 {
        buffer
    } else {
        let mut pixels = Vec::with_capacity((info.width * info.height * 4) as usize);
        for chunk in buffer.as_chunks::<3>().0 {
            pixels.extend_from_slice(chunk);
            pixels.push(255);
        }
        pixels
    };
    Ok(Image {
        width: info.width,
        height: info.height,
        pixels,
    })
}

fn write_png(path: &Path, image: &Image) -> Result<(), GoldenError> {
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)?;
    }
    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|error| GoldenError::Png(error.to_string()))?;
    writer
        .write_image_data(&image.pixels)
        .map_err(|error| GoldenError::Png(error.to_string()))
}

/// How far a capture may sit from its baseline and still match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Allowance {
    /// How far a channel of any pixel may move.
    pub rounding: u8,
    /// How many pixels may move further than that.
    pub strays: u64,
    /// How far a channel of such a pixel may move.
    pub stray_ceiling: u8,
}

/// What the phone's compared pictures are held to.
///
/// Every compared picture is drawn flat, and drawn flat, repeated runs match
/// their baselines to a channel's rounding nearly every time. Now and then
/// the rasteriser draws the anti-aliased edge of a rounded border a few
/// levels differently: once on CI, eight pixels on a card's four corners
/// moved by up to four levels in a picture that had stopped changing. So a
/// few pixels may move further than rounding, but only a little. Noise is
/// small in both ways at once and a change somebody made is not: a mark gone
/// missing moves its pixels far (and a needs-you dot alone is some 450
/// pixels), while a colour token that drifted moves thousands of pixels.
/// Slack of 12 levels over 600 pixels once hid a misplaced mask, which is
/// why neither number is generous. The component snapshots hold the same
/// numbers in `RoundingImageDiff.swift`.
pub const ALLOWANCE: Allowance = Allowance {
    rounding: 1,
    strays: 64,
    stray_ceiling: 8,
};

/// Compares two captures, retains the pair, and writes a difference only when
/// the comparison fails.
///
/// Failing on what nobody could see would train everybody to update
/// baselines without looking, so the comparison takes an [`Allowance`].
///
/// Pixels under the simulator's system chrome are never counted, whatever
/// they hold: that part of the picture is the system's, not the app's.
pub fn diff(
    expected: &Path,
    actual: &Path,
    out: &Path,
    allowance: Allowance,
    system_chrome: &[SystemChrome],
) -> Result<GoldenVerdict, GoldenError> {
    if !actual.is_file() {
        return Ok(GoldenVerdict::CaptureFailed(format!(
            "{} was never written",
            actual.display()
        )));
    }
    std::fs::create_dir_all(out)?;
    let taken = read_png(actual)?;
    std::fs::copy(actual, out.join("actual.png"))?;
    if !expected.is_file() {
        return Ok(GoldenVerdict::MissingBaseline);
    }
    let baseline = read_png(expected)?;
    std::fs::copy(expected, out.join("expected.png"))?;
    if baseline.width != taken.width || baseline.height != taken.height {
        return Ok(GoldenVerdict::SizeMismatch {
            expected: (baseline.width, baseline.height),
            actual: (taken.width, taken.height),
        });
    }

    // Most captures agree exactly. Compare the normalized bytes first; neither
    // the allowance nor excluded chrome can turn identical pixels into a
    // failure.
    if baseline.pixels == taken.pixels {
        return Ok(GoldenVerdict::Same);
    }
    let mut differences = Vec::new();
    let mut largest = 0;
    for (index, (expected, actual)) in baseline
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(taken.pixels.as_chunks::<4>().0)
        .enumerate()
    {
        if expected == actual {
            continue;
        }
        let (x, y) = (index as u32 % taken.width, index as u32 / taken.width);
        if system_chrome.iter().any(|chrome| chrome.covers(x, y)) {
            continue;
        }
        let moved = expected
            .iter()
            .zip(actual)
            .map(|(expected, actual)| expected.abs_diff(*actual))
            .max()
            .unwrap_or(0);
        if moved > allowance.rounding {
            differences.push(index);
            largest = largest.max(moved);
        }
    }
    if differences.is_empty() {
        return Ok(GoldenVerdict::Same);
    }
    let pixels = differences.len() as u64;
    if pixels > allowance.strays || largest > allowance.stray_ceiling {
        // Only failures need a picture. Red marks changed pixels; blue marks
        // system chrome that was excluded, whether or not it changed.
        let mut marked = Image {
            width: taken.width,
            height: taken.height,
            pixels: taken.pixels,
        };
        for (index, pixel) in marked.pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = (index as u32 % taken.width, index as u32 / taken.width);
            if system_chrome.iter().any(|chrome| chrome.covers(x, y)) {
                pixel[0] /= 4;
                pixel[1] = pixel[1] / 4 + 40;
                pixel[2] = pixel[2] / 4 + 150;
                pixel[3] = 255;
            } else {
                for channel in &mut pixel[..3] {
                    *channel = *channel / 3 + 40;
                }
            }
        }
        for index in &differences {
            marked.pixels[index * 4..index * 4 + 4].copy_from_slice(&[255, 32, 32, 255]);
        }
        write_png(&out.join("diff.png"), &marked)?;
        let first = differences[0] as u32;
        return Ok(GoldenVerdict::Different {
            pixels,
            largest,
            first: (first % taken.width, first / taken.width),
        });
    }
    Ok(GoldenVerdict::Close { pixels, largest })
}

// MARK: - The command

const MANIFEST: &str = "apps/apple/Goldens/manifest.json";
const OUT: &str = "target/ios/goldens";

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(2).collect();
    match arguments.first().map(String::as_str) {
        Some("diff") => diff_command(&arguments[1..]),
        _ => {
            eprintln!(
                "usage: xtask golden diff --expected PNG --actual PNG [--out DIR] \
                 [--simulator NAME] [--tolerance N] [--max-differing N] [--max-delta N] \
                 [--mask X,Y,W,H]..."
            );
            std::process::exit(2);
        }
    }
}

fn value(arguments: &[String], name: &str) -> Option<String> {
    arguments
        .iter()
        .position(|argument| argument == name)
        .and_then(|at| arguments.get(at + 1))
        .cloned()
}

fn diff_command(arguments: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let expected = value(arguments, "--expected").ok_or("--expected names the baseline PNG")?;
    let actual = value(arguments, "--actual").ok_or("--actual names the capture")?;
    let out = value(arguments, "--out").unwrap_or_else(|| format!("{OUT}/diff"));
    // The allowance every phone suite compares under, unless a flag names
    // another for a comparison made by hand.
    let allowance = Allowance {
        rounding: value(arguments, "--tolerance")
            .map(|text| text.parse())
            .transpose()?
            .unwrap_or(ALLOWANCE.rounding),
        strays: value(arguments, "--max-differing")
            .map(|text| text.parse())
            .transpose()?
            .unwrap_or(ALLOWANCE.strays),
        stray_ceiling: value(arguments, "--max-delta")
            .map(|text| text.parse())
            .transpose()?
            .unwrap_or(ALLOWANCE.stray_ceiling),
    };
    // The chrome of a named pinned simulator, so a pair of files compares the
    // way the run compares them; without one, every pixel counts.
    let mut chrome = match value(arguments, "--simulator") {
        Some(name) => GoldenManifest::read(Path::new(MANIFEST))?
            .simulators
            .get(&name)
            .ok_or_else(|| format!("the manifest does not declare simulator {name}"))?
            .system_chrome
            .clone(),
        None => Vec::new(),
    };
    // A journey's screen can carry text that moves with the run (an age, a
    // scratch path); the driver names each such rectangle in pixels.
    chrome.extend(masks(arguments)?);
    let verdict = diff(
        Path::new(&expected),
        Path::new(&actual),
        Path::new(&out),
        allowance,
        &chrome,
    )?;
    println!("{verdict}");
    if verdict.passed() {
        Ok(())
    } else {
        Err(format!("{expected} and {actual}: {verdict}").into())
    }
}

/// Every `--mask x,y,width,height` rectangle, in the capture's pixels.
fn masks(arguments: &[String]) -> Result<Vec<SystemChrome>, String> {
    arguments
        .iter()
        .enumerate()
        .filter(|(_, argument)| *argument == "--mask")
        .map(|(at, _)| {
            let text = arguments
                .get(at + 1)
                .ok_or("--mask names x,y,width,height")?;
            let numbers = text
                .split(',')
                .map(str::parse)
                .collect::<Result<Vec<u32>, _>>()
                .map_err(|_| format!("--mask {text} is not x,y,width,height"))?;
            let [x, y, width, height] = numbers[..] else {
                return Err(format!("--mask {text} is not x,y,width,height"));
            };
            Ok(SystemChrome {
                what: "volatile text the driver masked".into(),
                x,
                y,
                width,
                height,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, width: u32, height: u32, colour: [u8; 4]) {
        let image = Image {
            width,
            height,
            pixels: colour
                .iter()
                .cycle()
                .take((width * height * 4) as usize)
                .copied()
                .collect(),
        };
        write_png(path, &image).expect("the test image is written");
    }

    /// Rounding and nothing past it.
    fn strict(rounding: u8) -> Allowance {
        Allowance {
            rounding,
            strays: 0,
            stray_ceiling: 0,
        }
    }

    /// A flat picture with `count` pixels, from the first, moved by `by` in
    /// the red channel.
    fn write_with_strays(path: &Path, colour: [u8; 4], count: usize, by: u8) {
        let (width, height) = (16, 16);
        let mut pixels = Vec::with_capacity(width * height * 4);
        for _ in 0..width * height {
            pixels.extend_from_slice(&colour);
        }
        for pixel in pixels.as_chunks_mut::<4>().0.iter_mut().take(count) {
            pixel[0] += by;
        }
        write_png(
            path,
            &Image {
                width: width as u32,
                height: height as u32,
                pixels,
            },
        )
        .expect("a PNG");
    }

    #[test]
    fn a_few_pixels_a_little_past_rounding_match_and_are_counted() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let expected = room.path().join("expected.png");
        let actual = room.path().join("actual.png");
        write(&expected, 16, 16, [10, 20, 30, 255]);
        write_with_strays(&actual, [10, 20, 30, 255], 64, 8);
        let out = room.path().join("out");
        let verdict = diff(&expected, &actual, &out, ALLOWANCE, &[]).expect("a verdict");
        assert_eq!(
            verdict,
            GoldenVerdict::Close {
                pixels: 64,
                largest: 8
            }
        );
        assert!(verdict.passed());
        assert!(!out.join("diff.png").exists());
    }

    #[test]
    fn one_pixel_more_than_the_allowance_differs() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let expected = room.path().join("expected.png");
        let actual = room.path().join("actual.png");
        write(&expected, 16, 16, [10, 20, 30, 255]);
        write_with_strays(&actual, [10, 20, 30, 255], 65, 2);
        let out = room.path().join("out");
        assert_eq!(
            diff(&expected, &actual, &out, ALLOWANCE, &[]).expect("a verdict"),
            GoldenVerdict::Different {
                pixels: 65,
                largest: 2,
                first: (0, 0)
            }
        );
        assert!(out.join("diff.png").is_file());
    }

    #[test]
    fn one_pixel_moved_past_the_ceiling_differs() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let expected = room.path().join("expected.png");
        let actual = room.path().join("actual.png");
        write(&expected, 16, 16, [10, 20, 30, 255]);
        write_with_strays(&actual, [10, 20, 30, 255], 1, 9);
        assert_eq!(
            diff(&expected, &actual, &room.path().join("out"), ALLOWANCE, &[]).expect("a verdict"),
            GoldenVerdict::Different {
                pixels: 1,
                largest: 9,
                first: (0, 0)
            }
        );
    }

    #[test]
    fn rounding_everywhere_spends_none_of_the_allowance() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let expected = room.path().join("expected.png");
        let actual = room.path().join("actual.png");
        write(&expected, 16, 16, [10, 20, 30, 255]);
        write(&actual, 16, 16, [11, 19, 31, 255]);
        assert_eq!(
            diff(&expected, &actual, &room.path().join("out"), ALLOWANCE, &[]).expect("a verdict"),
            GoldenVerdict::Same
        );
    }

    #[test]
    fn the_same_capture_twice_is_the_same() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let expected = room.path().join("expected.png");
        let actual = room.path().join("actual.png");
        write(&expected, 4, 4, [10, 20, 30, 255]);
        write(&actual, 4, 4, [10, 20, 30, 255]);
        let out = room.path().join("out");
        assert_eq!(
            diff(&expected, &actual, &out, strict(2), &[]).expect("a verdict"),
            GoldenVerdict::Same
        );
        assert!(out.join("expected.png").is_file());
        assert!(out.join("actual.png").is_file());
        assert!(!out.join("diff.png").exists());
    }

    #[test]
    fn a_change_too_small_to_see_is_inside_the_tolerance() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let expected = room.path().join("expected.png");
        let actual = room.path().join("actual.png");
        write(&expected, 4, 4, [10, 20, 30, 255]);
        write(&actual, 4, 4, [11, 21, 31, 255]);
        assert_eq!(
            diff(&expected, &actual, &room.path().join("out"), strict(2), &[]).expect("a verdict"),
            GoldenVerdict::Same
        );
    }

    #[test]
    fn a_visible_change_is_reported_with_where_it_starts() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let expected = room.path().join("expected.png");
        let actual = room.path().join("actual.png");
        write(&expected, 4, 4, [10, 20, 30, 255]);
        write(&actual, 4, 4, [200, 20, 30, 255]);
        match diff(&expected, &actual, &room.path().join("out"), strict(2), &[]).expect("a verdict")
        {
            GoldenVerdict::Different { pixels, first, .. } => {
                assert_eq!(pixels, 16);
                assert_eq!(first, (0, 0));
            }
            other => panic!("expected a difference, got {other}"),
        }
        assert!(room.path().join("out/diff.png").is_file());
    }

    #[test]
    fn a_capture_of_another_size_is_not_compared_pixel_by_pixel() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let expected = room.path().join("expected.png");
        let actual = room.path().join("actual.png");
        write(&expected, 4, 4, [10, 20, 30, 255]);
        write(&actual, 8, 4, [10, 20, 30, 255]);
        assert_eq!(
            diff(&expected, &actual, &room.path().join("out"), strict(2), &[]).expect("a verdict"),
            GoldenVerdict::SizeMismatch {
                expected: (4, 4),
                actual: (8, 4)
            }
        );
    }

    #[test]
    fn a_screen_that_has_never_been_locked_says_so() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let actual = room.path().join("actual.png");
        write(&actual, 4, 4, [10, 20, 30, 255]);
        assert_eq!(
            diff(
                &room.path().join("nothing.png"),
                &actual,
                &room.path().join("out"),
                strict(2),
                &[]
            )
            .expect("a verdict"),
            GoldenVerdict::MissingBaseline
        );
    }

    #[test]
    fn a_capture_that_never_happened_says_so() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let expected = room.path().join("expected.png");
        write(&expected, 4, 4, [10, 20, 30, 255]);
        let verdict = diff(
            &expected,
            &room.path().join("nothing.png"),
            &room.path().join("out"),
            strict(2),
            &[],
        )
        .expect("a verdict");
        assert!(
            matches!(verdict, GoldenVerdict::CaptureFailed(_)),
            "expected a failed capture, got {verdict}"
        );
    }

    /// Paints one pixel of a solid image another colour.
    fn write_with_speck(path: &Path, width: u32, height: u32, colour: [u8; 4], at: (u32, u32)) {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            pixels.extend_from_slice(&colour);
        }
        let index = ((at.1 * width + at.0) * 4) as usize;
        pixels[index..index + 4].copy_from_slice(&[250, 250, 250, 255]);
        write_png(
            path,
            &Image {
                width,
                height,
                pixels,
            },
        )
        .expect("a PNG");
    }

    fn bar() -> SystemChrome {
        SystemChrome {
            what: "home indicator".into(),
            x: 1,
            y: 2,
            width: 2,
            height: 1,
        }
    }

    #[test]
    fn masks_are_read_from_every_mask_argument() {
        let arguments: Vec<String> = [
            "--expected",
            "a.png",
            "--mask",
            "1,2,3,4",
            "--mask",
            "0,0,10,20",
        ]
        .map(String::from)
        .to_vec();
        let read = masks(&arguments).unwrap();
        assert_eq!(read.len(), 2);
        assert!(read[0].covers(1, 2) && read[0].covers(3, 5) && !read[0].covers(4, 2));
        assert!(read[1].covers(9, 19) && !read[1].covers(10, 0));
        let bad: Vec<String> = ["--mask", "1,2,3"].map(String::from).to_vec();
        assert!(masks(&bad).is_err());
    }

    #[test]
    fn a_difference_under_system_chrome_is_not_a_difference() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let expected = room.path().join("expected.png");
        let actual = room.path().join("actual.png");
        write(&expected, 4, 4, [10, 20, 30, 255]);
        write_with_speck(&actual, 4, 4, [10, 20, 30, 255], (2, 2));
        let out = room.path().join("out");
        assert_eq!(
            diff(&expected, &actual, &out, strict(2), &[]).expect("a verdict"),
            GoldenVerdict::Different {
                pixels: 1,
                largest: 240,
                first: (2, 2)
            }
        );
        assert_eq!(
            diff(&expected, &actual, &out, strict(2), &[bar()]).expect("a verdict"),
            GoldenVerdict::Same
        );
    }

    #[test]
    fn a_difference_beside_system_chrome_still_counts() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let expected = room.path().join("expected.png");
        let actual = room.path().join("actual.png");
        write(&expected, 4, 4, [10, 20, 30, 255]);
        write_with_speck(&actual, 4, 4, [10, 20, 30, 255], (3, 2));
        let out = room.path().join("out");
        assert_eq!(
            diff(&expected, &actual, &out, strict(2), &[bar()]).expect("a verdict"),
            GoldenVerdict::Different {
                pixels: 1,
                largest: 240,
                first: (3, 2)
            }
        );
        // The chrome is washed blue in the difference image whether or not
        // anything under it differed, so what was not compared is visible.
        let marked = read_png(&out.join("diff.png")).expect("a difference image");
        let at = ((2 * 4 + 1) * 4) as usize;
        assert!(marked.pixels[at + 2] > marked.pixels[at] + 100);
    }

    #[test]
    fn a_picture_that_does_not_say_what_it_shows_is_refused() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let path = room.path().join("manifest.json");
        std::fs::write(
            &path,
            r#"{"simulators": {}, "topology": "t.json",
                "components": [{"id": "row.prompt", "shows": " "}], "screens": []}"#,
        )
        .expect("a manifest");
        let error = GoldenManifest::read(&path).expect_err("a silent picture");
        assert!(error.to_string().contains("does not say"), "{error}");
    }

    #[test]
    fn a_name_used_twice_is_refused() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let path = room.path().join("manifest.json");
        std::fs::write(
            &path,
            r#"{"simulators": {}, "topology": "t.json", "components": [],
                "screens": [{"id": "fleet", "shows": "a"}, {"id": "fleet", "shows": "b"}]}"#,
        )
        .expect("a manifest");
        let error = GoldenManifest::read(&path).expect_err("a repeated name");
        assert!(error.to_string().contains("named twice"), "{error}");
    }

    const GOLDENS: &str = "../../apps/apple/Goldens";
    const SNAPSHOTS: &str =
        "../../apps/apple/AmuxComponentSnapshotTests/__Snapshots__/ComponentSnapshotTests";

    /// Every golden a screen owns, without its appearance, named as the
    /// golden driver writes them.
    fn goldens(screen: &GoldenScreen) -> Vec<String> {
        if screen.frames.is_empty() {
            vec![screen.id.clone()]
        } else {
            screen
                .frames
                .iter()
                .map(|frame| format!("{}-{}", screen.id, frame.name))
                .collect()
        }
    }

    fn committed() -> GoldenManifest {
        GoldenManifest::read(&Path::new(GOLDENS).join("manifest.json")).expect("the manifest")
    }

    fn files(directory: &str, extension: &str) -> BTreeSet<String> {
        std::fs::read_dir(directory)
            .expect("the baselines")
            .map(|entry| {
                entry
                    .expect("a file")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.ends_with(extension))
            .collect()
    }

    /// The component snapshot suite names a baseline after its example id,
    /// with every character that is not a letter or digit made a dash.
    fn snapshot(id: &str, appearance: &str) -> String {
        format!("components.{}-{appearance}.png", id.replace('.', "-"))
    }

    /// Every component the catalogue pins has a sentence, and the catalogue
    /// vocabulary is all there: each row kind, the activity line, each ask
    /// body, the escape, not-confirmed with resend and discard, the exited
    /// composer's resume and the strip once per provider kind.
    #[test]
    fn the_components_are_the_catalogue_with_a_sentence_each() {
        let manifest = committed();
        let named: BTreeSet<String> = manifest
            .components
            .iter()
            .flat_map(|component| {
                ["light", "dark"].map(|appearance| snapshot(&component.id, appearance))
            })
            .collect();
        assert_eq!(
            named,
            files(SNAPSHOTS, ".png"),
            "components and their baselines disagree"
        );
        let ids: BTreeSet<&str> = manifest.components.iter().map(|c| c.id.as_str()).collect();
        let required = [
            "row.prompt",
            "row.prose",
            "row.prose-working-note",
            "row.thinking",
            "row.tool-call",
            "row.file-change",
            "row.command",
            "row.explore",
            "row.run",
            "row.subagent-running",
            "row.subagent-done",
            "row.background",
            "row.image",
            "row.slash-output",
            "row.turn-end",
            "row.stopped",
            "row.compaction",
            "row.error",
            "row.model-switch",
            "row.boundary",
            "row.agent-message",
            "row.auto-review",
            "row.unrecognized",
            "row.ask-question",
            "row.ask-plan-approved",
            "row.ask-form",
            "row.ask-link",
            "row.ask-grant",
            "row.ask-unanswerable",
            "composer.working",
            "composer.thinking",
            "composer.retrying",
            "ask.permission-command",
            "ask.permission-edit",
            "ask.permission-tool",
            "ask.codex-command",
            "ask.question-single",
            "ask.questions-step",
            "ask.question-secret",
            "ask.plan",
            "ask.form",
            "ask.link",
            "ask.access",
            "ask.unanswerable",
            "composer.not-confirmed",
            "composer.resume",
            "composer.strip-claude-pty",
            "composer.strip-claude-sdk",
            "composer.strip-codex",
            "composer.dictation-denied",
        ];
        let missing: Vec<&str> = required
            .into_iter()
            .filter(|id| !ids.contains(id))
            .collect();
        assert!(missing.is_empty(), "the catalogue lost {missing:?}");
    }

    /// The whole screens the phone is held to, each with an image per
    /// appearance and one element geometry, and nothing else in the
    /// directory: a baseline nobody claims is a screen dropped with its
    /// photographs left behind.
    #[test]
    fn the_screens_are_owed_and_every_baseline_is_claimed() {
        let manifest = committed();
        let ids: Vec<&str> = manifest
            .screens
            .iter()
            .map(|screen| screen.id.as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "pairing",
                "hosts",
                "fleet",
                "chat-turn",
                "claude-sdk-ask-question",
                "ask-escape",
                "composer-chips",
                "origin-rewind"
            ]
        );
        let rewind = manifest
            .screens
            .iter()
            .find(|screen| screen.id == "origin-rewind")
            .unwrap();
        assert_eq!(
            goldens(rewind),
            ["origin-rewind-before", "origin-rewind-after"]
        );
        let mut claimed = BTreeSet::new();
        for screen in &manifest.screens {
            for golden in goldens(screen) {
                claimed.insert(format!("{golden}.light.png"));
                claimed.insert(format!("{golden}.dark.png"));
                claimed.insert(format!("{golden}.elements.txt"));
            }
        }
        let mut present = files(GOLDENS, ".png");
        present.extend(files(GOLDENS, ".txt"));
        assert_eq!(present, claimed);
        assert!(
            Path::new("../..").join(&manifest.topology).is_file(),
            "the topology is committed"
        );

        // The pinned phone is declared with the chrome the comparison must
        // look past.
        let golden = manifest.simulators.get("golden").expect("the golden phone");
        let named: Vec<&str> = golden
            .system_chrome
            .iter()
            .map(|chrome| chrome.what.split(':').next().unwrap_or(""))
            .collect();
        assert_eq!(
            named,
            [
                "status bar clock",
                "status bar indicators",
                "home indicator"
            ]
        );
        assert_eq!(
            manifest.simulators.len(),
            1,
            "one pinned phone photographs every screen"
        );
    }
}
