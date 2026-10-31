//! Semantic colour tokens shared by every TUI surface.

use ratatui::style::{Color, Modifier, Style};

/// The colour capability selected once at the terminal boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColorMode {
    TrueColor,
    Ansi,
}

/// The user's colour-capability preference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorPreference {
    #[default]
    Auto,
    TrueColor,
    Ansi,
}

/// One semantic colour with truecolor and 16-colour terminal faces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    pub rgb: (u8, u8, u8),
    pub ansi: Color,
    /// Whether this colour is the terminal's own, in which case amux names
    /// it rather than painting it.
    ///
    /// The two are not the same even when the bytes match. A terminal whose
    /// ground is translucent, blurred, or an image reports a solid colour
    /// for it; painting that colour back is an opaque rectangle over the
    /// person's wallpaper, while leaving the cell reset shows the wallpaper.
    /// The same holds for body text under a terminal that recolours it.
    pub inherited: bool,
}

impl Token {
    const fn new(rgb: (u8, u8, u8), ansi: Color) -> Self {
        Self {
            rgb,
            ansi,
            inherited: false,
        }
    }

    /// A colour amux does not choose: the terminal's, named by leaving the
    /// cell at its default. `rgb` still carries what that default is, so
    /// contrast arithmetic over the rest of the palette stays honest.
    const fn borrowed(rgb: (u8, u8, u8)) -> Self {
        Self {
            rgb,
            ansi: Color::Reset,
            inherited: true,
        }
    }

    /// Resolve this token without making painters branch on terminal support.
    pub fn resolve(self, mode: ColorMode) -> Color {
        if self.inherited {
            return Color::Reset;
        }
        match mode {
            ColorMode::TrueColor => Color::Rgb(self.rgb.0, self.rgb.1, self.rgb.2),
            ColorMode::Ansi => self.ansi,
        }
    }
}

/// The complete semantic vocabulary available to TUI painters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tokens {
    pub background: Token,
    pub text: Token,
    pub muted: Token,
    /// A third ink, below muted: metadata read only when looked for.
    pub faint: Token,
    pub emphasis: Token,
    pub attention: Token,
    pub user_surface: Token,
    pub panel: Token,
    /// Section rules: a line that separates without being read.
    pub hairline: Token,
    pub focus: Token,
    pub code: Token,
    pub ok: Token,
    pub warning: Token,
    pub error: Token,
    pub diff_added_fg: Token,
    pub diff_added_bg: Token,
    pub diff_removed_fg: Token,
    pub diff_removed_bg: Token,
    pub diff_context: Token,
    pub diff_meta: Token,
}

/// What the terminal itself is painting with.
///
/// A terminal will answer three questions about the scheme it is running:
/// its ground, its text colour, and the sixteen colours its palette defines.
/// That is the whole of what a program can learn about a scheme it did not
/// choose, and this type is that answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalColors {
    pub background: (u8, u8, u8),
    pub foreground: (u8, u8, u8),
    /// Slots 0-15 in the usual order: black, red, green, yellow, blue,
    /// magenta, cyan, white, then the eight bright ones.
    pub ansi: [(u8, u8, u8); 16],
}

/// The accents a palette has to source from somewhere, whichever end it
/// sources them from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Semantic {
    Ok,
    Warning,
    Error,
    Attention,
    Code,
    Focus,
}

impl Semantic {
    /// The plain and bright ANSI slots carrying this meaning.
    const fn slots(self) -> (usize, usize) {
        match self {
            Self::Error => (1, 9),
            Self::Ok => (2, 10),
            Self::Warning => (3, 11),
            Self::Attention => (4, 12),
            Self::Focus => (5, 13),
            Self::Code => (6, 14),
        }
    }
}

impl TerminalColors {
    /// Whether words are being drawn light-on-dark.
    ///
    /// Asked of the ground rather than of a setting, because the ground is
    /// the thing every derived colour is measured from.
    pub fn is_dark(&self) -> bool {
        luminance(self.background) < 0.18
    }

    /// The colour this scheme means by a semantic accent.
    ///
    /// Each meaning lives in two slots, plain and bright, and schemes
    /// disagree about what the bright half is for: most put brighter
    /// versions of the same hues there, but Solarized spends it on greys, so
    /// taking the bright slot on a hunch yields a grey where green was
    /// wanted. Prefer whichever of the pair still carries a hue, and decide
    /// between two hued candidates by which one reads better on this ground.
    fn hue(&self, semantic: Semantic) -> Token {
        /// The saturation below which a slot has stopped being a colour and
        /// is just a step on the scheme's grey ramp.
        const COLOURED: f64 = 0.2;

        let weigh = |slot: usize| {
            let rgb = self.ansi[slot];
            let (_, saturation, _) = to_hsl(rgb);
            (saturation, contrast(rgb, self.background), slot)
        };
        let (plain, bright) = semantic.slots();
        let (plain, bright) = (weigh(plain), weigh(bright));
        let chosen = match (plain.0 >= COLOURED, bright.0 >= COLOURED) {
            (true, false) => plain,
            (false, true) => bright,
            _ if bright.1 > plain.1 => bright,
            _ => plain,
        };
        Token::new(self.ansi[chosen.2], slot_face(chosen.2))
    }
}

/// The 16-colour face for a palette slot, so a derived colour degrades to
/// the slot it actually came from rather than to whichever conventional
/// face its RGB happens to sit nearest.
const fn slot_face(slot: usize) -> Color {
    match slot {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::Gray,
        8 => Color::DarkGray,
        9 => Color::LightRed,
        10 => Color::LightGreen,
        11 => Color::LightYellow,
        12 => Color::LightBlue,
        13 => Color::LightMagenta,
        14 => Color::LightCyan,
        _ => Color::White,
    }
}

const WHITE: (u8, u8, u8) = (255, 255, 255);
const BLACK: (u8, u8, u8) = (0, 0, 0);

/// Move `from` a fraction of the way toward `toward`.
fn mix(from: (u8, u8, u8), toward: (u8, u8, u8), amount: f64) -> (u8, u8, u8) {
    let step = |from: u8, toward: u8| {
        (f64::from(from) * (1.0 - amount) + f64::from(toward) * amount).round() as u8
    };
    (
        step(from.0, toward.0),
        step(from.1, toward.1),
        step(from.2, toward.2),
    )
}

/// A colour amux worked out for itself rather than borrowed.
fn computed(rgb: (u8, u8, u8)) -> Token {
    Token::new(rgb, nearest_ansi(rgb))
}

/// The provenance of a resolved palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThemeName {
    Dark,
    Light,
    /// A palette derived from what the terminal reported about itself.
    Adopted,
}

/// A complete palette resolved in one terminal colour mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub tokens: Tokens,
    pub mode: ColorMode,
    pub name: ThemeName,
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark(ColorMode::TrueColor)
    }
}

impl Theme {
    /// amux's own dark palette: a cool near-black that is not quite
    /// black, one clearly lighter surface for the person's own words, and
    /// soft accents so nothing in the feed shouts. As in a terminal's own
    /// palette, the attention ink ("needs you") is a blue and code a cyan.
    /// Tuned by eye at 120x40 against the working Claude and Codex
    /// screens; every hex is amux's, borrowed from no published scheme.
    pub const fn dark(mode: ColorMode) -> Self {
        Self {
            tokens: Tokens {
                background: Token::new((15, 18, 22), Color::Black),
                text: Token::new((223, 227, 232), Color::White),
                muted: Token::new((138, 144, 160), Color::DarkGray),
                faint: Token::new((96, 102, 116), Color::DarkGray),
                emphasis: Token::new((242, 244, 247), Color::White),
                // The sixteen conventional blues are too dark to read on
                // black, so in that face attention takes light cyan.
                attention: Token::new((108, 160, 232), Color::LightCyan),
                user_surface: Token::new((24, 32, 40), Color::Black),
                panel: Token::new((23, 27, 34), Color::Blue),
                hairline: Token::new((44, 50, 58), Color::DarkGray),
                focus: Token::new((156, 140, 214), Color::Magenta),
                code: Token::new((86, 182, 194), Color::Cyan),
                ok: Token::new((134, 184, 122), Color::Green),
                warning: Token::new((210, 162, 76), Color::Yellow),
                error: Token::new((222, 123, 132), Color::LightRed),
                diff_added_fg: Token::new((143, 203, 138), Color::Black),
                diff_added_bg: Token::new((22, 38, 27), Color::Green),
                diff_removed_fg: Token::new((224, 141, 149), Color::White),
                diff_removed_bg: Token::new((44, 26, 30), Color::Red),
                diff_context: Token::new((195, 201, 212), Color::White),
                diff_meta: Token::new((110, 118, 134), Color::DarkGray),
            },
            mode,
            name: ThemeName::Dark,
        }
    }

    /// amux's own light palette: a warm off-white that is easier to sit
    /// in front of than pure white, the same blue attention and cyan code
    /// darkened until they hold their own on paper, and diff tints kept pale enough that a
    /// hunk still reads as text.
    pub const fn light(mode: ColorMode) -> Self {
        Self {
            tokens: Tokens {
                background: Token::new((250, 250, 248), Color::White),
                text: Token::new((42, 46, 56), Color::Black),
                muted: Token::new((106, 112, 128), Color::Black),
                faint: Token::new((134, 139, 150), Color::DarkGray),
                emphasis: Token::new((21, 24, 31), Color::Black),
                attention: Token::new((38, 94, 168), Color::Blue),
                user_surface: Token::new((236, 241, 243), Color::Cyan),
                panel: Token::new((240, 240, 238), Color::Gray),
                hairline: Token::new((220, 223, 228), Color::Gray),
                focus: Token::new((109, 78, 156), Color::Magenta),
                // Conventional cyan does not read on paper; in the sixteen
                // faces code shares attention's blue, in a different context.
                code: Token::new((14, 112, 122), Color::Blue),
                ok: Token::new((47, 122, 68), Color::Green),
                warning: Token::new((138, 91, 16), Color::Yellow),
                error: Token::new((168, 50, 68), Color::Red),
                diff_added_fg: Token::new((34, 107, 51), Color::Black),
                diff_added_bg: Token::new((227, 242, 229), Color::Green),
                diff_removed_fg: Token::new((158, 47, 63), Color::White),
                diff_removed_bg: Token::new((250, 228, 230), Color::Red),
                diff_context: Token::new((58, 63, 76), Color::Black),
                diff_meta: Token::new((117, 122, 136), Color::Black),
            },
            mode,
            name: ThemeName::Light,
        }
    }

    /// A whole palette derived from what the terminal reported about itself.
    ///
    /// This is the answer to "can amux belong to your terminal *and* still
    /// draw a readable diff". A terminal will tell a program its ground, its
    /// text colour and the sixteen colours its scheme defines; what it will
    /// not give you is a surface one step off the ground, or that ground
    /// nudged toward green. Those are not missing colours, though — they are
    /// missing *relationships*, and once the ground is a known value they are
    /// arithmetic. So the ground and the body text stay the terminal's own,
    /// untouched and unpainted, and everything amux needs beyond them is
    /// computed from that pair and repaired until it reads.
    ///
    /// Then the palette is repaired: a person's terminal was not chosen
    /// with amux's screens in mind, and a scheme whose text sits a few steps
    /// off its ground would otherwise leave the composer and the status line
    /// unreadable. Only HSL lightness moves, so the scheme's hues survive.
    ///
    /// The accents and the diff tints are the terminal's own, so amux
    /// agrees with everything else in the person's session about what red
    /// means. Keeping amux's own hues on a borrowed ground was considered
    /// and rejected: a colour is a shared vocabulary inside one terminal,
    /// and a program that opts out of it teaches a second one.
    pub fn from_terminal(terminal: TerminalColors, mode: ColorMode) -> Self {
        let bg = terminal.background;
        let fg = terminal.foreground;
        let dark = terminal.is_dark();

        // Surfaces are the ground moved toward the text, which lightens a
        // dark terminal and darkens a light one without either case being
        // special. `user_surface` is meant to be seen as a block; `panel`
        // only has to separate.
        let surface = |amount: f64| computed(mix(bg, fg, amount));
        // De-emphasis is the reverse: the text pulled back toward the ground
        // far enough to recede and no further.
        let recede = |amount: f64| computed(mix(fg, bg, amount));

        let hue = |semantic: Semantic| terminal.hue(semantic);

        let tokens = Tokens {
            background: Token::borrowed(bg),
            text: Token::borrowed(fg),
            diff_context: Token::borrowed(fg),
            emphasis: computed(mix(fg, if dark { WHITE } else { BLACK }, 0.4)),
            muted: recede(0.42),
            faint: recede(0.6),
            diff_meta: recede(0.52),
            // Enough to read as a block and no more. The person's own
            // message is the one thing on the screen they do not need to
            // find, so its surface states where it is rather than shouting.
            user_surface: surface(0.10),
            panel: surface(0.055),
            hairline: surface(0.2),
            // Both tints start as the same plain surface and are tinted by
            // the repair pass with this palette's own success and error
            // hues, so a hunk is green-on-your-green and red-on-your-red
            // rather than a colour amux picked for you.
            diff_added_bg: surface(0.055),
            diff_removed_bg: surface(0.055),
            ok: hue(Semantic::Ok),
            diff_added_fg: hue(Semantic::Ok),
            warning: hue(Semantic::Warning),
            error: hue(Semantic::Error),
            diff_removed_fg: hue(Semantic::Error),
            attention: hue(Semantic::Attention),
            code: hue(Semantic::Code),
            focus: hue(Semantic::Focus),
        };

        let mut tokens = tokens;
        // The three borrowed tokens are protected by being borrowed: repair
        // leaves the terminal's own colours alone.
        make_readable(&mut tokens, &Faces::of_terminal(&terminal));

        Self {
            tokens,
            mode,
            name: ThemeName::Adopted,
        }
    }

    fn color(self, token: Token) -> Color {
        token.resolve(self.mode)
    }

    // These semantic styles are the whole vocabulary the chat and the fleet
    // may paint from: naming a token here rather than a colour literal is
    // what makes a palette swap a data change and a token rename a compile
    // error. Every one of them has a caller.
    /// Body text.
    pub(crate) fn text(self) -> Style {
        Style::default().fg(self.color(self.tokens.text))
    }

    /// De-emphasis: markers, rules, continuations, and hints.
    pub(crate) fn muted(self) -> Style {
        Style::default().fg(self.color(self.tokens.muted))
    }

    /// Metadata read only when looked for: ages, projects, hosts, counts.
    pub(crate) fn faint(self) -> Style {
        Style::default().fg(self.color(self.tokens.faint))
    }

    /// The brightest ink without weight: what the eye should land on first,
    /// like a name in a list.
    pub(crate) fn bright(self) -> Style {
        Style::default().fg(self.color(self.tokens.emphasis))
    }

    /// Markdown emphasis (bold) and headings.
    pub(crate) fn emphasis(self) -> Style {
        Style::default()
            .fg(self.color(self.tokens.emphasis))
            .add_modifier(Modifier::BOLD)
    }

    /// Inline code and fenced code blocks.
    pub(crate) fn code(self) -> Style {
        Style::default().fg(self.color(self.tokens.code))
    }

    /// A code token's colour: each kind takes one of the palette's hues,
    /// the terminal's own when it reported them, and every one has been
    /// lifted to read on the background. Names and punctuation stay in the
    /// reading ink and comments recede, so the colour is on what differs.
    pub(crate) fn syntax(self, kind: crate::highlight::TokenKind) -> Style {
        use crate::highlight::TokenKind;
        let token = match kind {
            TokenKind::Plain => self.tokens.text,
            TokenKind::Keyword => self.tokens.focus,
            TokenKind::String | TokenKind::Inserted => self.tokens.ok,
            TokenKind::Number => self.tokens.warning,
            TokenKind::Type => self.tokens.code,
            TokenKind::Function => self.tokens.attention,
            TokenKind::Comment => self.tokens.faint,
            TokenKind::Deleted => self.tokens.error,
        };
        Style::default().fg(self.color(token))
    }

    /// Success accents (`✔`).
    pub(crate) fn ok(self) -> Style {
        Style::default().fg(self.color(self.tokens.ok))
    }

    /// Warning ink (the terminal's yellow, made readable): something in the
    /// way that is not a failure, like a usage limit reached.
    pub(crate) fn warning(self) -> Style {
        Style::default().fg(self.color(self.tokens.warning))
    }

    /// Failure accents (`✗`, API errors, failed sends).
    pub(crate) fn error(self) -> Style {
        Style::default().fg(self.color(self.tokens.error))
    }

    /// The filled user-message and composer surface.
    pub(crate) fn user_surface(self) -> Style {
        self.text().bg(self.color(self.tokens.user_surface))
    }

    /// A highlighted row's surface: the terminal's own ground moved a step
    /// toward its text. None when amux does not know that ground, or cannot
    /// paint an exact shade of it, where a highlight falls back to weight
    /// and a mark rather than borrowing a colour of its own.
    pub(crate) fn row_surface(self) -> Option<Style> {
        (self.name == ThemeName::Adopted && self.mode == ColorMode::TrueColor)
            .then(|| Style::default().bg(self.color(self.tokens.user_surface)))
    }

    /// An attachment chip set into the person's own words: on the
    /// highlight surface in the draft, a step further from the ground
    /// inside a message block, which already sits on that surface.
    pub(crate) fn chip(self) -> Style {
        self.text().bg(self.color(self.tokens.user_surface))
    }

    pub(crate) fn chip_raised(self) -> Style {
        self.text().bg(self.color(self.tokens.hairline))
    }

    /// A section rule: separates without being read.
    pub(crate) fn hairline(self) -> Style {
        Style::default().fg(self.color(self.tokens.hairline))
    }

    /// Attention on the bare page: "needs you", an open ask's mark, the
    /// composer's edge.
    pub(crate) fn attention(self) -> Style {
        Style::default().fg(self.color(self.tokens.attention))
    }

    /// The bar marking the focused feed block.
    pub(crate) fn focus_bar(self) -> Style {
        Style::default().fg(self.color(self.tokens.focus))
    }

    /// Added diff content and its tint.
    pub(crate) fn diff_added(self) -> Style {
        Style::default()
            .fg(self.color(self.tokens.diff_added_fg))
            .bg(self.color(self.tokens.diff_added_bg))
    }

    /// Removed diff content and its tint.
    pub(crate) fn diff_removed(self) -> Style {
        Style::default()
            .fg(self.color(self.tokens.diff_removed_fg))
            .bg(self.color(self.tokens.diff_removed_bg))
    }

    /// Unchanged diff content. It carries no surface of its own: on a
    /// panel it takes the panel's, and on the bare page a tint that stopped
    /// where the text stopped would read as a sticker behind each line.
    pub(crate) fn diff_context(self) -> Style {
        Style::default().fg(self.color(self.tokens.diff_context))
    }

    /// Convert a rendered cell style into its semantic style-map class.
    pub fn classify(self, style: Style) -> char {
        // A drawn cell's unset colours read back as Reset: the terminal's
        // own default, which is no colour of the theme's.
        let fg = style.fg.filter(|color| *color != Color::Reset);
        let bg = style.bg.filter(|color| *color != Color::Reset);

        if fg == Some(self.color(self.tokens.attention))
            && bg == Some(self.color(self.tokens.user_surface))
        {
            return 'A';
        }
        if fg == Some(self.color(self.tokens.focus)) {
            return 'F';
        }
        if bg == Some(self.color(self.tokens.diff_added_bg)) {
            return '+';
        }
        if bg == Some(self.color(self.tokens.diff_removed_bg)) {
            return '-';
        }
        if bg == Some(self.color(self.tokens.user_surface)) {
            return 'U';
        }
        if bg == Some(self.color(self.tokens.panel)) {
            return 'P';
        }

        // A row that fills to the width sets the background token
        // explicitly, so a foreground token over it is still a plain row
        // wearing one colour — not a surface.
        let plain_bg = bg.is_none() || bg == Some(self.color(self.tokens.background));

        if style.add_modifier.contains(Modifier::BOLD)
            && fg == Some(self.color(self.tokens.emphasis))
        {
            return 'e';
        }
        if style.add_modifier.contains(Modifier::ITALIC) && fg == Some(self.color(self.tokens.text))
        {
            return 'i';
        }

        for (token, class) in [
            (self.tokens.muted, 'm'),
            (self.tokens.code, 'c'),
            (self.tokens.attention, 'a'),
            (self.tokens.ok, 'o'),
            (self.tokens.warning, 'w'),
            (self.tokens.error, 'x'),
            (self.tokens.diff_meta, 'M'),
        ] {
            if fg == Some(self.color(token)) && plain_bg && style.add_modifier.is_empty() {
                return class;
            }
        }

        let plain_fg = fg.is_none()
            || fg == Some(self.color(self.tokens.text))
            || fg == Some(self.color(self.tokens.diff_context));
        if plain_fg && plain_bg && style.add_modifier.is_empty() && style.sub_modifier.is_empty() {
            '.'
        } else {
            '?'
        }
    }
}

/// The contrast a derived palette has to reach before amux paints words
/// with it. Body copy is held above the WCAG AAA threshold so a terminal's
/// scheme still reads like the shipped ones; labels, accents and status
/// colours are held to AA; rules and hunk metadata are decoration
/// and only have to be visible.
const READABLE_BODY: f64 = 7.0;
const READABLE_LABEL: f64 = 4.5;
const READABLE_TRIM: f64 = 3.0;

/// How far apart two foregrounds have to be before a reader can see that
/// one of them is emphasised.
const SEPARATION: f64 = 1.25;

/// Repair a derived palette so every foreground is legible on the surfaces
/// it can land on.
///
/// A terminal's scheme is chosen for whatever the person runs in it. amux
/// paints labels, a composer and a status line as well, and a scheme whose
/// text sits a few steps above its ground leaves all of that unreadable —
/// worse once sixteen-colour degradation rounds both of them to the same
/// terminal face. Rather than reject such a scheme, lift each foreground away
/// from its surfaces until it clears the floor above, moving only HSL
/// lightness so the scheme's own hues and saturation survive exactly. A
/// scheme that is already readable comes through untouched.
fn make_readable(tokens: &mut Tokens, faces: &Faces) {
    // Tint the diff surfaces with the palette's own success and error hues,
    // and give each tint the saturated terminal face its hue rounds to so a
    // diff still reads as a diff in sixteen colours.
    tokens.diff_added_bg = tinted(tokens.diff_added_bg, tokens.ok, faces);
    tokens.diff_removed_bg = tinted(tokens.diff_removed_bg, tokens.error, faces);

    // A scheme's block surfaces are shades of its background, and rounding
    // each one to its own terminal face turns a barely-there shade into a
    // loud grey band and then constrains every foreground that lands on it.
    // Anything this close to the background is not meant to read as a colour,
    // so let it share the background's face and disappear the way it does in
    // truecolor.
    if shade_of(tokens.user_surface, tokens.background) {
        tokens.user_surface.ansi = tokens.background.ansi;
    }
    if shade_of(tokens.panel, tokens.background) {
        tokens.panel.ansi = tokens.background.ansi;
    }

    let surfaces = [tokens.background, tokens.user_surface, tokens.panel];
    let added = [tokens.diff_added_bg];
    let removed = [tokens.diff_removed_bg];
    let hunk = [
        tokens.panel,
        tokens.background,
        tokens.diff_added_bg,
        tokens.diff_removed_bg,
    ];

    // `on_ground` says the surfaces are the ground and its shades; anything
    // else a foreground is measured against is a tint that already wears a
    // hue of its own.
    let lift = |token: &mut Token, on: &[Token], floor: f64, on_ground: bool| {
        let against: Vec<(Token, f64)> = on.iter().map(|surface| (*surface, floor)).collect();
        *token = readable_token(*token, &against, faces, on_ground);
    };

    lift(&mut tokens.text, &surfaces, READABLE_BODY, true);
    lift(&mut tokens.diff_context, &hunk, READABLE_BODY, false);
    lift(&mut tokens.muted, &surfaces, READABLE_LABEL, true);
    lift(&mut tokens.faint, &surfaces, READABLE_TRIM, true);
    lift(&mut tokens.attention, &surfaces, READABLE_LABEL, true);
    lift(&mut tokens.focus, &surfaces, READABLE_LABEL, true);
    lift(&mut tokens.code, &surfaces, READABLE_LABEL, true);
    lift(&mut tokens.ok, &surfaces, READABLE_LABEL, true);
    lift(&mut tokens.warning, &surfaces, READABLE_LABEL, true);
    lift(&mut tokens.error, &surfaces, READABLE_LABEL, true);
    lift(&mut tokens.diff_added_fg, &added, READABLE_LABEL, false);
    lift(&mut tokens.diff_removed_fg, &removed, READABLE_LABEL, false);
    lift(&mut tokens.diff_meta, &hunk, READABLE_TRIM, false);

    // Emphasis is only emphasis if it out-reads the body it interrupts, and
    // lifting two flat greys to the same floor would land them on the same
    // colour, so hold it away from the repaired text as well.
    let against = [
        (tokens.background, READABLE_BODY),
        (tokens.user_surface, READABLE_BODY),
        (tokens.panel, READABLE_BODY),
        (tokens.text, SEPARATION),
    ];
    tokens.emphasis = readable_token(tokens.emphasis, &against, faces, true);
}

/// Mix a hue into a surface far enough to be seen without turning the
/// surface into a foreground. Unlike the block surfaces above, a tint is
/// meant to be seen, so it keeps a face of its own even when the mix is
/// pale: the nearest face by identity, which is the tint's hue at whichever
/// end of the ramp the mix landed on.
fn tinted(surface: Token, hue: Token, faces: &Faces) -> Token {
    const MIX: f64 = 0.22;
    let blend = |surface: u8, hue: u8| {
        (f64::from(surface) * (1.0 - MIX) + f64::from(hue) * MIX).round() as u8
    };
    let rgb = (
        blend(surface.rgb.0, hue.rgb.0),
        blend(surface.rgb.1, hue.rgb.1),
        blend(surface.rgb.2, hue.rgb.2),
    );
    let ansi = faces
        .0
        .into_iter()
        .min_by(|left, right| {
            face_identity(rgb, left.1)
                .partial_cmp(&face_identity(rgb, right.1))
                .expect("face cost is finite")
        })
        .map(|(face, _)| face)
        .expect("ANSI palette is non-empty");
    Token {
        rgb,
        ansi,
        // A tint is paint by definition: it is the one thing on the screen
        // that is deliberately not the ground it sits on.
        inherited: false,
    }
}

/// Whether one surface is close enough to the background to read as the same
/// surface rather than as a block of its own.
fn shade_of(surface: Token, background: Token) -> bool {
    const SHADE: f64 = 1.5;
    contrast(surface.rgb, background.rgb) < SHADE
}

/// Lift one token until it clears every floor it was given, in both colour
/// modes. `on_ground` says the surfaces are the ground and its shades rather
/// than a tint.
fn readable_token(token: Token, against: &[(Token, f64)], faces: &Faces, on_ground: bool) -> Token {
    if token.inherited {
        return token;
    }
    let rgb = lift_lightness(token.rgb, against);
    // A token that *is* one of this palette's slots keeps that slot in
    // sixteen colours. Repair moves lightness, and in sixteen colours there
    // is no lightness to move — only a swap to a different slot, which
    // changes what the colour means. On this terminal red means what its
    // red slot paints, in `git diff` and everywhere else in the session,
    // and a person whose scheme has a dim red chose that; amux swapping it
    // for yellow would be the one program in the session disagreeing.
    //
    // That holds on the ground. On a tint — removed text on the removed
    // tint — the surface already wears the hue, and the same slot laid over
    // it is red on red; there the face that reads best wins.
    let is_slot = token.ansi != Color::Reset && faces.rgb_of_face(token.ansi) == token.rgb;
    Token {
        rgb,
        ansi: if is_slot && on_ground {
            token.ansi
        } else {
            readable_face(rgb, against, faces)
        },
        inherited: false,
    }
}

/// The smallest ratio of achieved contrast to demanded contrast across every
/// surface: at or above 1.0 the colour is readable everywhere it can land.
fn margin(
    candidate: (u8, u8, u8),
    against: &[(Token, f64)],
    surface: impl Fn(Token) -> (u8, u8, u8),
) -> f64 {
    against
        .iter()
        .map(|(token, floor)| contrast(candidate, surface(*token)) / floor)
        .fold(f64::INFINITY, f64::min)
}

/// Move a colour's HSL lightness — and nothing else — until it is readable.
fn lift_lightness(rgb: (u8, u8, u8), against: &[(Token, f64)]) -> (u8, u8, u8) {
    let reach = |candidate| margin(candidate, against, |token| token.rgb);
    if reach(rgb) >= 1.0 {
        return rgb;
    }
    let (hue, saturation, lightness) = to_hsl(rgb);
    let toward = if reach(from_hsl(hue, saturation, 1.0)) >= reach(from_hsl(hue, saturation, 0.0)) {
        1.0
    } else {
        0.0
    };
    // Binary search the least movement that reads, keeping `far` as the end
    // known to be no worse than the extreme this scheme can reach.
    let (mut near, mut far) = (lightness, toward);
    for _ in 0..24 {
        let middle = (near + far) / 2.0;
        if reach(from_hsl(hue, saturation, middle)) >= 1.0 {
            far = middle;
        } else {
            near = middle;
        }
    }
    from_hsl(hue, saturation, far)
}

/// Choose the sixteen-colour face that keeps the most of a repaired colour.
///
/// Rounding each colour to its nearest RGB neighbour on its own is what made
/// derived palettes vanish on a monochrome terminal — a whole dark ramp
/// collapses onto black — and nearest-RGB also throws hues away, because a
/// half-saturated lavender is closer to grey than to magenta. Score every face
/// on what it preserves instead: a saturated colour has to stay a coloured
/// face and a neutral one a grey face, then the nearer hue wins, then the
/// nearer lightness. Falling short of the contrast floor is a penalty rather
/// than a veto, since in this mode the terminal owns the colours it actually
/// paints and the floor is measured against conventional values; a face may
/// win by a small shortfall if it keeps the hue, never by a large one.
fn readable_face(rgb: (u8, u8, u8), against: &[(Token, f64)], faces: &Faces) -> Color {
    const SHORTFALL: f64 = 4.0;
    let reach = |candidate: (u8, u8, u8)| margin(candidate, against, |token| faces.rgb(token));

    // Against a coloured surface — a diff tint — the surface already says what
    // the row is, and a second hue laid over it only clashes. Take the face
    // that reads best there and let it be neutral, which is what the shipped
    // palettes do with their own tints.
    if against
        .iter()
        .any(|(surface, _)| chromatic(faces.rgb(*surface)))
    {
        return faces
            .0
            .into_iter()
            .max_by(|left, right| {
                reach(left.1)
                    .partial_cmp(&reach(right.1))
                    .expect("contrast is finite")
                    .then_with(|| {
                        face_identity(rgb, right.1)
                            .partial_cmp(&face_identity(rgb, left.1))
                            .expect("face cost is finite")
                    })
            })
            .map(|(face, _)| face)
            .expect("ANSI palette is non-empty");
    }

    let cost = |candidate: (u8, u8, u8)| {
        face_identity(rgb, candidate) + (1.0 - reach(candidate)).max(0.0) * SHORTFALL
    };
    faces
        .0
        .into_iter()
        .min_by(|left, right| {
            cost(left.1)
                .partial_cmp(&cost(right.1))
                .expect("face cost is finite")
        })
        .map(|(face, _)| face)
        .expect("ANSI palette is non-empty")
}

/// Whether a colour reads as a hue rather than as a grey.
fn chromatic(rgb: (u8, u8, u8)) -> bool {
    /// Below this, a colour reads as a grey rather than as a hue.
    const CHROMATIC: f64 = 0.2;
    to_hsl(rgb).1 >= CHROMATIC
}

/// How much of a colour's identity a terminal face gives up, in the same
/// units as the contrast shortfall above.
fn face_identity(rgb: (u8, u8, u8), face: (u8, u8, u8)) -> f64 {
    let (hue, _, lightness) = to_hsl(rgb);
    let (face_hue, _, face_lightness) = to_hsl(face);
    let coloured = chromatic(rgb);
    let face_coloured = chromatic(face);
    let class = f64::from(u8::from(coloured != face_coloured));
    let gap = if coloured && face_coloured {
        let gap = (hue - face_hue).abs().rem_euclid(360.0);
        gap.min(360.0 - gap) / 180.0
    } else {
        0.0
    };
    class + gap * 1.5 + (lightness - face_lightness).abs() * 0.5
}

/// Relative luminance, per WCAG 2.
pub(crate) fn luminance(rgb: (u8, u8, u8)) -> f64 {
    fn channel(value: u8) -> f64 {
        let value = f64::from(value) / 255.0;
        if value <= 0.040_45 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(rgb.0) + 0.7152 * channel(rgb.1) + 0.0722 * channel(rgb.2)
}

/// The WCAG 2 contrast ratio between two colours, from 1.0 to 21.0.
pub fn contrast(left: (u8, u8, u8), right: (u8, u8, u8)) -> f64 {
    let (left, right) = (luminance(left), luminance(right));
    let (lighter, darker) = if left > right {
        (left, right)
    } else {
        (right, left)
    };
    (lighter + 0.05) / (darker + 0.05)
}

fn to_hsl(rgb: (u8, u8, u8)) -> (f64, f64, f64) {
    let (red, green, blue) = (
        f64::from(rgb.0) / 255.0,
        f64::from(rgb.1) / 255.0,
        f64::from(rgb.2) / 255.0,
    );
    let max = red.max(green).max(blue);
    let min = red.min(green).min(blue);
    let lightness = (max + min) / 2.0;
    let range = max - min;
    if range <= f64::EPSILON {
        return (0.0, 0.0, lightness);
    }
    let saturation = range / (1.0 - (2.0 * lightness - 1.0).abs());
    let hue = if max == red {
        ((green - blue) / range).rem_euclid(6.0)
    } else if max == green {
        (blue - red) / range + 2.0
    } else {
        (red - green) / range + 4.0
    };
    (hue * 60.0, saturation, lightness)
}

fn from_hsl(hue: f64, saturation: f64, lightness: f64) -> (u8, u8, u8) {
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let sector = hue.rem_euclid(360.0) / 60.0;
    let middle = chroma * (1.0 - (sector.rem_euclid(2.0) - 1.0).abs());
    let (red, green, blue) = match sector as u32 {
        0 => (chroma, middle, 0.0),
        1 => (middle, chroma, 0.0),
        2 => (0.0, chroma, middle),
        3 => (0.0, middle, chroma),
        4 => (middle, 0.0, chroma),
        _ => (chroma, 0.0, middle),
    };
    let base = lightness - chroma / 2.0;
    let quantize = |value: f64| ((value + base) * 255.0).round().clamp(0.0, 255.0) as u8;
    (quantize(red), quantize(green), quantize(blue))
}

/// The sixteen terminal faces at their conventional RGB values, which is the
/// only ground amux has for reasoning about contrast in a mode where the
/// terminal, not amux, owns the actual colours.
const ANSI_FACES: [(Color, (u8, u8, u8)); 16] = [
    (Color::Black, (0, 0, 0)),
    (Color::Red, (128, 0, 0)),
    (Color::Green, (0, 128, 0)),
    (Color::Yellow, (128, 128, 0)),
    (Color::Blue, (0, 0, 128)),
    (Color::Magenta, (128, 0, 128)),
    (Color::Cyan, (0, 128, 128)),
    (Color::Gray, (192, 192, 192)),
    (Color::DarkGray, (128, 128, 128)),
    (Color::LightRed, (255, 0, 0)),
    (Color::LightGreen, (0, 255, 0)),
    (Color::LightYellow, (255, 255, 0)),
    (Color::LightBlue, (0, 0, 255)),
    (Color::LightMagenta, (255, 0, 255)),
    (Color::LightCyan, (0, 255, 255)),
    (Color::White, (255, 255, 255)),
];

/// The conventional RGB value of one terminal face.
#[cfg(test)]
fn ansi_rgb(face: Color) -> (u8, u8, u8) {
    Faces(ANSI_FACES).rgb_of_face(face)
}

/// What the sixteen faces actually paint as, for reasoning about contrast
/// in the mode where the terminal owns the colours.
///
/// A palette derived from a terminal knows exactly what each slot paints
/// as, because the terminal said so — measuring it against the conventional
/// values would judge Solarized's yellow-green `Green` as if it were pure
/// green, and pick a face for its ground that the ground is nothing like.
#[derive(Clone, Copy)]
struct Faces([(Color, (u8, u8, u8)); 16]);

impl Faces {
    /// The faces as one terminal reported them.
    fn of_terminal(terminal: &TerminalColors) -> Self {
        let mut faces = ANSI_FACES;
        for (slot, face) in faces.iter_mut().enumerate() {
            *face = (slot_face(slot), terminal.ansi[slot]);
        }
        Self(faces)
    }

    fn rgb_of_face(&self, face: Color) -> (u8, u8, u8) {
        self.0
            .iter()
            .find(|(candidate, _)| *candidate == face)
            .map(|(_, rgb)| *rgb)
            .unwrap_or((0, 0, 0))
    }

    /// What a token paints as in sixteen colours. A borrowed token is left
    /// at the terminal's default, which is the colour it reported.
    fn rgb(&self, token: Token) -> (u8, u8, u8) {
        if token.inherited || token.ansi == Color::Reset {
            token.rgb
        } else {
            self.rgb_of_face(token.ansi)
        }
    }
}

/// Choose the nearest named 16-colour terminal face by squared RGB distance.
pub fn nearest_ansi(rgb: (u8, u8, u8)) -> Color {
    ANSI_FACES
        .into_iter()
        .min_by_key(|(_, candidate)| rgb_distance(rgb, *candidate))
        .map(|(color, _)| color)
        .expect("ANSI palette is non-empty")
}

fn rgb_distance(left: (u8, u8, u8), right: (u8, u8, u8)) -> u32 {
    let red = i32::from(left.0) - i32::from(right.0);
    let green = i32::from(left.1) - i32::from(right.1);
    let blue = i32::from(left.2) - i32::from(right.2);
    (red * red + green * green + blue * blue) as u32
}

/// Resolve the terminal colour mode from an explicit preference and captured
/// environment strings. `TERM` is accepted at this boundary for diagnostics,
/// but auto mode deliberately trusts only the standard `COLORTERM` signal.
pub fn detect_color_mode(
    preference: ColorPreference,
    colorterm: Option<&str>,
    _term: Option<&str>,
    no_color: bool,
) -> ColorMode {
    match preference {
        ColorPreference::TrueColor => ColorMode::TrueColor,
        ColorPreference::Ansi => ColorMode::Ansi,
        ColorPreference::Auto => {
            let advertised = colorterm.is_some_and(|value| {
                value.eq_ignore_ascii_case("truecolor") || value.eq_ignore_ascii_case("24bit")
            });
            if advertised && !no_color {
                ColorMode::TrueColor
            } else {
                ColorMode::Ansi
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::buffer::Buffer;

    use super::*;
    use crate::fixtures::Named;

    fn render_named(state: Named, theme: Theme) -> Buffer {
        state.render(theme)
    }

    /// The attention ink on the person's message surface.
    fn attention_on_surface(theme: Theme) -> Style {
        Style::default()
            .fg(theme.color(theme.tokens.attention))
            .bg(theme.color(theme.tokens.user_surface))
    }

    #[test]
    fn classify_recognizes_semantic_styles() {
        let theme = Theme::default();
        assert_eq!(theme.classify(Style::default()), '.');
        assert_eq!(theme.classify(theme.text()), '.');
        assert_eq!(theme.classify(theme.muted()), 'm');
        assert_eq!(theme.classify(theme.emphasis()), 'e');
        let italic = Style::default()
            .fg(theme.color(theme.tokens.text))
            .add_modifier(Modifier::ITALIC);
        assert_eq!(theme.classify(italic), 'i');
        assert_eq!(theme.classify(theme.code()), 'c');
        assert_eq!(theme.classify(theme.ok()), 'o');
        assert_eq!(theme.classify(theme.warning()), 'w');
        assert_eq!(theme.classify(theme.error()), 'x');
        assert_eq!(theme.classify(theme.user_surface()), 'U');
        assert_eq!(theme.classify(attention_on_surface(theme)), 'A');
        assert_eq!(theme.classify(theme.focus_bar()), 'F');
        assert_eq!(theme.classify(theme.diff_added()), '+');
        assert_eq!(theme.classify(theme.diff_removed()), '-');
        assert_eq!(theme.classify(theme.diff_context()), '.');
        assert_eq!(theme.classify(theme.attention()), 'a');
    }

    #[test]
    fn classify_rejects_stray_literal_styles() {
        let theme = Theme::default();
        let stray = Style::default().fg(Color::Rgb(1, 2, 3));
        assert_eq!(theme.classify(stray), '?');
    }

    #[test]
    fn classify_resolves_ansi_faces_without_confusing_attention_and_code() {
        for theme in [Theme::dark(ColorMode::Ansi), Theme::light(ColorMode::Ansi)] {
            assert_eq!(theme.classify(attention_on_surface(theme)), 'A');
            assert_eq!(theme.classify(theme.code()), 'c');
            assert_eq!(theme.classify(theme.diff_added()), '+');
            assert_eq!(theme.classify(theme.diff_removed()), '-');
            assert_eq!(theme.classify(theme.user_surface()), 'U');
        }
    }

    #[test]
    fn auto_detection_requires_a_truecolor_advertisement() {
        assert_eq!(
            detect_color_mode(
                ColorPreference::Auto,
                Some("truecolor"),
                Some("xterm"),
                false
            ),
            ColorMode::TrueColor
        );
        assert_eq!(
            detect_color_mode(ColorPreference::Auto, Some("24BIT"), None, false),
            ColorMode::TrueColor
        );
        assert_eq!(
            detect_color_mode(ColorPreference::Auto, None, Some("xterm-truecolor"), false),
            ColorMode::Ansi
        );
        assert_eq!(
            detect_color_mode(ColorPreference::Auto, Some("yes"), Some("xterm"), false),
            ColorMode::Ansi
        );
    }

    #[test]
    fn forced_detection_ignores_environment_capability() {
        assert_eq!(
            detect_color_mode(ColorPreference::TrueColor, None, Some("dumb"), true),
            ColorMode::TrueColor
        );
        assert_eq!(
            detect_color_mode(
                ColorPreference::Ansi,
                Some("truecolor"),
                Some("xterm"),
                false
            ),
            ColorMode::Ansi
        );
    }

    #[test]
    fn no_color_disables_auto_truecolor() {
        assert_eq!(
            detect_color_mode(
                ColorPreference::Auto,
                Some("truecolor"),
                Some("xterm"),
                true
            ),
            ColorMode::Ansi
        );
    }

    #[test]
    fn nearest_ansi_uses_named_terminal_colours() {
        assert_eq!(nearest_ansi((1, 2, 3)), Color::Black);
        assert_eq!(nearest_ansi((250, 10, 10)), Color::LightRed);
        assert_eq!(nearest_ansi((10, 240, 245)), Color::LightCyan);
        assert_eq!(nearest_ansi((248, 248, 248)), Color::White);
    }

    /// The palettes are only a claim until something paints with them: walk
    /// the cells of a real frame and hold every painted glyph to the floor
    /// below.
    #[test]
    fn every_theme_paints_a_readable_frame() {
        for theme in [
            Theme::dark(ColorMode::TrueColor),
            Theme::dark(ColorMode::Ansi),
            Theme::light(ColorMode::TrueColor),
            Theme::light(ColorMode::Ansi),
        ] {
            let surface = match theme.mode {
                ColorMode::TrueColor => theme.tokens.background.rgb,
                ColorMode::Ansi => ansi_rgb(theme.tokens.background.ansi),
            };
            let painted = |color: Color| match color {
                Color::Rgb(red, green, blue) => (red, green, blue),
                Color::Reset => surface,
                face => ansi_rgb(face),
            };
            for state in [
                Named::ClaudeWorking,
                Named::ClaudePermissionAsk,
                Named::CodexApproval,
            ] {
                let buffer = render_named(state, theme);
                for cell in buffer.content() {
                    if cell.symbol().trim().is_empty() {
                        continue;
                    }
                    // A hairline rule separates and is never read, so it sits
                    // below the floor on purpose.
                    if cell.symbol() == "─" && cell.fg == theme.color(theme.tokens.hairline) {
                        continue;
                    }
                    let ratio = contrast(painted(cell.fg), painted(cell.bg));
                    assert!(
                        ratio >= READABLE_TRIM,
                        "{} {:?} {:?} paints {:?} at {ratio:.1}:1, {:?} on {:?}",
                        state.name(),
                        theme.name,
                        theme.mode,
                        cell.symbol(),
                        cell.fg,
                        cell.bg
                    );
                }
            }
        }
    }

    // --- the two shipped palettes -------------------------------------

    fn rgb(hex: u32) -> (u8, u8, u8) {
        ((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
    }

    /// Two schemes a terminal might report, chosen because they disagree
    /// about everything a derivation has to cope with: one is dark with a
    /// conventional bright half, the other is light and spends its bright
    /// half on greys.
    fn terminal_schemes() -> Vec<(&'static str, TerminalColors)> {
        let one_dark = TerminalColors {
            background: rgb(0x282c34),
            foreground: rgb(0xabb2bf),
            ansi: [
                0x282c34, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xabb2bf,
                0x5c6370, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xffffff,
            ]
            .map(rgb),
        };
        let solarized_light = TerminalColors {
            background: rgb(0xfdf6e3),
            foreground: rgb(0x657b83),
            ansi: [
                0x073642, 0xdc322f, 0x859900, 0xb58900, 0x268bd2, 0xd33682, 0x2aa198, 0xeee8d5,
                0x002b36, 0xcb4b16, 0x586e75, 0x657b83, 0x839496, 0x6c71c4, 0x93a1a1, 0xfdf6e3,
            ]
            .map(rgb),
        };
        vec![("one dark", one_dark), ("solarized light", solarized_light)]
    }

    /// Every palette amux can end up drawing with: the two it ships and
    /// one derived from each terminal scheme above.
    fn every_palette() -> Vec<Theme> {
        let mut palettes = vec![
            Theme::dark(ColorMode::TrueColor),
            Theme::light(ColorMode::TrueColor),
        ];
        palettes.extend(
            terminal_schemes()
                .into_iter()
                .map(|(_, terminal)| Theme::from_terminal(terminal, ColorMode::TrueColor)),
        );
        palettes
    }

    /// A palette derived from a terminal leaves the ground and the text as
    /// the terminal's own — named, not painted — and takes its accents from
    /// the terminal's slots, so amux's green is the terminal's green.
    #[test]
    fn a_terminal_palette_borrows_its_ground_and_text_and_takes_its_own_hues() {
        for (name, terminal) in terminal_schemes() {
            let theme = Theme::from_terminal(terminal, ColorMode::TrueColor);
            assert_eq!(theme.name, ThemeName::Adopted);
            for token in [theme.tokens.background, theme.tokens.text] {
                assert_eq!(
                    token.resolve(ColorMode::TrueColor),
                    Color::Reset,
                    "{name}: a borrowed colour is painted rather than left to the terminal"
                );
            }
            assert_eq!(theme.tokens.background.rgb, terminal.background);
            assert_eq!(theme.tokens.text.rgb, terminal.foreground);

            // Repair may move an accent's lightness, never its hue.
            let hue_of = |rgb: (u8, u8, u8)| to_hsl(rgb).0;
            let same_hue = |token: Token, slots: [usize; 2]| {
                slots.iter().any(|slot| {
                    let delta = (hue_of(token.rgb) - hue_of(terminal.ansi[*slot])).abs();
                    delta.min(360.0 - delta) < 5.0
                })
            };
            assert!(
                same_hue(theme.tokens.ok, [2, 10]),
                "{name}: ok is not the terminal's green"
            );
            assert!(
                same_hue(theme.tokens.error, [1, 9]),
                "{name}: error is not the terminal's red"
            );
            assert!(
                same_hue(theme.tokens.attention, [4, 12]),
                "{name}: attention is not the terminal's blue"
            );

            // The surfaces are steps off the ground, in the direction of the
            // text, so a light terminal gets darker surfaces and a dark one
            // lighter.
            let step = |surface: (u8, u8, u8)| luminance(surface) - luminance(terminal.background);
            for surface in [theme.tokens.user_surface.rgb, theme.tokens.panel.rgb] {
                assert!(
                    (step(surface) > 0.0) == terminal.is_dark(),
                    "{name}: a surface stepped away from the text"
                );
            }
        }
    }

    /// In sixteen colours a derived palette can only name the terminal's
    /// slots, so an accent keeps the slot it came from and every derived
    /// face is measured against what the terminal said that slot paints as,
    /// not against the conventional value it would have on some other
    /// terminal.
    #[test]
    fn a_terminal_palette_in_sixteen_colours_names_the_terminals_own_slots() {
        for (name, terminal) in terminal_schemes() {
            let theme = Theme::from_terminal(terminal, ColorMode::Ansi);
            let faces = Faces::of_terminal(&terminal);
            for (token, slots, what) in [
                (theme.tokens.ok, [2usize, 10], "ok"),
                (theme.tokens.error, [1, 9], "error"),
                (theme.tokens.attention, [4, 12], "attention"),
            ] {
                let face = token.resolve(ColorMode::Ansi);
                assert!(
                    slots.iter().any(|slot| slot_face(*slot) == face),
                    "{name}: {what} resolved to {face:?}, not the terminal's own slot"
                );
            }
            for (token, what, floor) in [
                (theme.tokens.emphasis, "emphasis", READABLE_LABEL),
                (theme.tokens.muted, "muted", READABLE_TRIM),
            ] {
                let ratio = contrast(faces.rgb(token), terminal.background);
                assert!(
                    ratio >= floor,
                    "{name}: {what} paints as {:?} on the ground, only {ratio:.2}:1",
                    token.resolve(ColorMode::Ansi)
                );
            }
            // On a tint the slot rule gives way: removed text on the removed
            // tint takes whichever of the sixteen faces reads best there,
            // rather than the terminal's red on the terminal's red.
            for (text, tint, what) in [
                (
                    theme.tokens.diff_removed_fg,
                    theme.tokens.diff_removed_bg,
                    "removed",
                ),
                (
                    theme.tokens.diff_added_fg,
                    theme.tokens.diff_added_bg,
                    "added",
                ),
            ] {
                let chosen = contrast(faces.rgb(text), faces.rgb(tint));
                let best = faces
                    .0
                    .iter()
                    .map(|(_, rgb)| contrast(*rgb, faces.rgb(tint)))
                    .fold(0.0, f64::max);
                assert!(
                    chosen >= best - 1e-9,
                    "{name}: {what} text paints as {:?} on its tint at {chosen:.2}:1; {best:.2}:1 was available",
                    text.resolve(ColorMode::Ansi)
                );
            }
        }
    }

    /// A derived palette leaves the ground and the text as the terminal
    /// chose them, so what it owes a reader is everything it computed: each
    /// derived foreground clears the repair floor on every surface it can
    /// land on.
    #[test]
    fn a_terminal_palette_repairs_what_it_derived() {
        for (name, terminal) in terminal_schemes() {
            let tokens = Theme::from_terminal(terminal, ColorMode::TrueColor).tokens;
            let surfaces = [tokens.background, tokens.user_surface, tokens.panel];
            let hunk = [
                tokens.panel,
                tokens.background,
                tokens.diff_added_bg,
                tokens.diff_removed_bg,
            ];
            let check = |token: Token, what: &str, on: &[Token], floor: f64| {
                for surface in on {
                    let ratio = contrast(token.rgb, surface.rgb);
                    assert!(
                        ratio >= floor,
                        "{name}: {what} on {:?} is only {ratio:.1}:1, floor {floor}",
                        surface.rgb
                    );
                }
            };
            check(tokens.emphasis, "emphasis", &surfaces, READABLE_BODY);
            for (token, what) in [
                (tokens.muted, "muted"),
                (tokens.attention, "attention"),
                (tokens.focus, "focus"),
                (tokens.code, "code"),
                (tokens.ok, "ok"),
                (tokens.warning, "warning"),
                (tokens.error, "error"),
            ] {
                check(token, what, &surfaces, READABLE_LABEL);
            }
            check(
                tokens.diff_added_fg,
                "added text",
                &[tokens.diff_added_bg],
                READABLE_LABEL,
            );
            check(
                tokens.diff_removed_fg,
                "removed text",
                &[tokens.diff_removed_bg],
                READABLE_LABEL,
            );
            check(tokens.diff_meta, "diff notes", &hunk, READABLE_TRIM);
        }
    }

    /// Body text has to be readable on every surface it can land on, in
    /// both shipped palettes — 4.5:1 is the WCAG AA threshold for ordinary
    /// text. A derived palette's text is the terminal's own and is not
    /// amux's to hold to a floor.
    #[test]
    fn palettes_keep_text_readable_on_every_surface() {
        for theme in [
            Theme::dark(ColorMode::TrueColor),
            Theme::light(ColorMode::TrueColor),
        ] {
            let text = theme.tokens.text.rgb;
            for (surface, name) in [
                (theme.tokens.background.rgb, "background"),
                (theme.tokens.user_surface.rgb, "user_surface"),
                (theme.tokens.panel.rgb, "panel"),
            ] {
                let ratio = contrast(text, surface);
                assert!(
                    ratio >= 4.5,
                    "{:?} text on {name} is only {ratio:.1}:1",
                    theme.name
                );
            }
            for (fg, bg, name) in [
                (
                    theme.tokens.diff_added_fg.rgb,
                    theme.tokens.diff_added_bg.rgb,
                    "diff added",
                ),
                (
                    theme.tokens.diff_removed_fg.rgb,
                    theme.tokens.diff_removed_bg.rgb,
                    "diff removed",
                ),
            ] {
                let ratio = contrast(fg, bg);
                assert!(ratio >= 4.5, "{:?} {name} is only {ratio:.1}:1", theme.name);
            }
        }
    }

    /// A tinted diff row has to be visibly a tint and not just the panel
    /// it sits on, and the two tints have to be told apart from each
    /// other at a glance.
    #[test]
    fn palettes_separate_the_diff_tints_from_the_panel() {
        for theme in every_palette() {
            let panel = theme.tokens.panel.rgb;
            let added = theme.tokens.diff_added_bg.rgb;
            let removed = theme.tokens.diff_removed_bg.rgb;
            assert_ne!(added, panel, "{:?} added tint is the panel", theme.name);
            assert_ne!(removed, panel, "{:?} removed tint is the panel", theme.name);
            assert_ne!(added, removed, "{:?} tints are the same", theme.name);
            assert_ne!(
                theme.tokens.user_surface.rgb, panel,
                "{:?} user surface is the panel",
                theme.name
            );
        }
    }

    /// The shipped palettes are amux's own. This locks the deliberate
    /// move off the borrowed hexes the provisional palette used, so a
    /// later tweak cannot quietly reintroduce a published scheme amux
    /// only supports as an imported theme file.
    #[test]
    fn palettes_borrow_no_published_scheme() {
        const BORROWED: [(u8, u8, u8); 6] = [
            (122, 162, 247),
            (125, 207, 255),
            (158, 206, 106),
            (224, 175, 104),
            (247, 118, 142),
            (187, 154, 247),
        ];
        for theme in [
            Theme::dark(ColorMode::TrueColor),
            Theme::light(ColorMode::TrueColor),
        ] {
            for token in [
                theme.tokens.background,
                theme.tokens.text,
                theme.tokens.muted,
                theme.tokens.emphasis,
                theme.tokens.attention,
                theme.tokens.user_surface,
                theme.tokens.panel,
                theme.tokens.focus,
                theme.tokens.code,
                theme.tokens.ok,
                theme.tokens.warning,
                theme.tokens.error,
                theme.tokens.diff_added_fg,
                theme.tokens.diff_removed_fg,
                theme.tokens.diff_context,
                theme.tokens.diff_meta,
            ] {
                assert!(
                    !BORROWED.contains(&token.rgb),
                    "{:?} still carries a borrowed colour {:?}",
                    theme.name,
                    token.rgb
                );
            }
        }
    }
}
