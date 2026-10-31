# amux-shot

`amux-shot` renders PNGs of the terminal's chat vocabulary: the same
buffers the component goldens in `crates/tui/tests/golden` compare as text
and style maps, rasterized so a reviewer can see them. It
does not start a PTY, connect to a daemon, or inspect the local terminal.
Every cell is 10×22 pixels, drawn with vendored JetBrains Mono faces and a
DejaVu Sans fallback; both fonts carry their open-source licenses beside
the assets.

Run it through the declared task from the repository root:

```sh
just shot -- render --out target/amux-shot/vocabulary
just shot -- render row_prose ask_form --theme light --out DIR
just shot -- render --theme dark --color ansi --out target/amux-shot/ansi
just shot -- frame <root>/reports/report-<stamp>
```

`render` writes one PNG per component and theme, `<name>.<theme>.png`, for
the components named (every one when none is; the names are the golden
files' under `crates/tui/tests/golden`). `frame` draws a terminal report's frozen screen from its `frame.json` into
`frame.png` beside it, so the client itself carries no fonts.

`amux-storybook` is the terminal client's storybook of whole screens, the
terminal stories listed in `tests/stories.json`: `just storybook` steps
through them in this terminal and `just storybook capture --out DIR` draws
each to text and PNG. See [the storybook](../../docs/TERMINAL.md#storybook).
