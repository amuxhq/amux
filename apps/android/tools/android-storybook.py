#!/usr/bin/env python3
"""Render the Android headless Storybook PNGs and generate its HTML gallery.

Invoked via `just android storybook [filter] [--no-daemon]`. Runs Robolectric
+ Roborazzi (`StorybookCaptureTest`) to render every matching story in Light
and Dark appearances into `target/android/storybook/png/`, validates that every
expected PNG is non-empty, and writes a self-contained static gallery at
`target/android/storybook/index.html`.

Each story also has a geometry report in `geometry/<id>.<appearance>.json`:
where its pieces sit and the alignment near-misses among them. The gallery
lists the light appearance's near-misses under each story and totals them at
the top. Like the pictures, the report is for reading and never fails a run.
"""

from __future__ import annotations

import argparse
from html import escape
import json
from pathlib import Path
import subprocess
import sys

REPO_ROOT = Path(__file__).resolve().parents[3]
DEFAULT_OUT_DIR = REPO_ROOT / "target" / "android" / "storybook"
GRADLE_WRAPPER = REPO_ROOT / "apps" / "android" / "tools" / "gradle-run.sh"
CAPTURE_TEST = "sh.amux.android.StorybookCaptureTest"


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Render Android Storybook PNGs and generate target/android/storybook/index.html.",
    )
    parser.add_argument(
        "filters",
        nargs="*",
        help="Optional story id or prefix filter(s), e.g. fleet.busy or hosts.",
    )
    parser.add_argument(
        "--out",
        type=Path,
        default=DEFAULT_OUT_DIR,
        help=f"Output directory (default: {DEFAULT_OUT_DIR}).",
    )
    parser.add_argument(
        "--no-daemon",
        action="store_true",
        help="Pass --no-daemon to Gradle.",
    )
    parser.add_argument(
        "--html-only",
        action="store_true",
        help="Skip Gradle rendering and regenerate index.html from existing manifest.json.",
    )
    return parser.parse_args(argv)


def build_gradle_command(
    out_dir: Path,
    filters: list[str],
    no_daemon: bool,
) -> list[str]:
    cmd = [
        "sh",
        str(GRADLE_WRAPPER),
        ":app:testDebugUnitTest",
        "--rerun",
        "--tests",
        CAPTURE_TEST,
        "-Damux.storybook.capture=true",
        f"-Damux.storybook.out={out_dir.resolve()}",
    ]
    if filters:
        joined = ",".join(filters)
        cmd.append(f"-Damux.storybook.filter={joined}")
    if no_daemon:
        cmd.append("--no-daemon")
    return cmd


def load_manifest(out_dir: Path) -> list[dict]:
    manifest_path = out_dir / "manifest.json"
    if not manifest_path.is_file():
        raise FileNotFoundError(f"missing storybook manifest at {manifest_path}")
    data = json.loads(manifest_path.read_text(encoding="utf-8"))
    stories = data.get("stories")
    if not isinstance(stories, list) or not stories:
        raise ValueError(f"storybook manifest {manifest_path} contains no stories")
    for entry in stories:
        for key in ("lightPng", "darkPng"):
            png_path = out_dir / entry[key]
            if not png_path.is_file() or png_path.stat().st_size == 0:
                raise ValueError(f"missing or empty storybook PNG: {png_path}")
    return stories


AREA_ORDER = ("Fleet", "New Agent", "Chat", "Asks", "Review", "Hosts", "Pairing", "Settings", "Report", "System")


def ordered_areas(stories: list[dict]) -> list[str]:
    present = {story.get("area", "Other") for story in stories}
    ordered = [area for area in AREA_ORDER if area in present]
    for story in stories:
        area = story.get("area", "Other")
        if area not in ordered:
            ordered.append(area)
    return ordered


GEOMETRY_RULES = {
    "row-centre": "Row centres differ",
    "near-column": "Edge just off a shared column",
    "tall-control": "Control drawn taller than 40 dp",
    "icon-inset": "Glyph inset from its column",
}


def load_flags(out_dir: Path | None, story: dict) -> list[dict] | None:
    """The light appearance's geometry flags, or None when there is no report."""
    path = story.get("lightGeometry")
    if out_dir is None or not path:
        return None
    try:
        data = json.loads((out_dir / path).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    flags = data.get("flags")
    return flags if isinstance(flags, list) else None


def render_flags(flags: list[dict] | None) -> str:
    if flags is None:
        return ""
    if not flags:
        return '<p class="geometry-none">No alignment near-misses.</p>'
    items = "".join(
        f'<li><span class="rule">{escape(GEOMETRY_RULES.get(str(f.get("rule")), str(f.get("rule"))))}</span> '
        f'{escape(str(f.get("detail", "")))}</li>'
        for f in sorted(flags, key=lambda f: (str(f.get("rule")), -float(f.get("offDp", 0))))
    )
    return (
        f'<details class="geometry"><summary>{len(flags)} alignment near-miss'
        f'{"" if len(flags) == 1 else "es"}</summary><ul>{items}</ul></details>'
    )


def render_geometry_summary(flags_by_story: dict[str, list[dict] | None]) -> str:
    reported = {sid: flags for sid, flags in flags_by_story.items() if flags is not None}
    if not reported:
        return ""
    totals = {rule: 0 for rule in GEOMETRY_RULES}
    for flags in reported.values():
        for flag in flags:
            rule = str(flag.get("rule"))
            totals[rule] = totals.get(rule, 0) + 1
    rule_rows = "".join(
        f"<tr><td>{escape(GEOMETRY_RULES.get(rule, rule))}</td><td>{count}</td></tr>"
        for rule, count in totals.items()
    )
    busiest = sorted(reported.items(), key=lambda kv: -len(kv[1]))[:10]
    story_rows = "".join(
        f'<tr><td><a href="#{escape(sid)}">{escape(sid)}</a></td><td>{len(flags)}</td></tr>'
        for sid, flags in busiest
        if flags
    )
    return f"""
    <section class="geometry-summary">
      <h2>Geometry</h2>
      <p>Alignment near-misses read from each story's semantics bounds (light appearance). A reading aid, never a gate; the full reports, with every piece's bounds, are in <code>geometry/</code>.</p>
      <div class="tables">
        <table><thead><tr><th>Rule</th><th>Flags</th></tr></thead><tbody>{rule_rows}</tbody></table>
        <table><thead><tr><th>Most flagged</th><th>Flags</th></tr></thead><tbody>{story_rows}</tbody></table>
      </div>
    </section>
    """


def render_html(stories: list[dict], out_dir: Path | None = None) -> str:
    areas_seen = ordered_areas(stories)
    flags_by_story = {str(story["id"]): load_flags(out_dir, story) for story in stories}

    tab_buttons = ['<button class="tab active" data-area="All">All ({})</button>'.format(len(stories))]
    for area in areas_seen:
        count = sum(1 for s in stories if s.get("area") == area)
        tab_buttons.append(
            f'<button class="tab" data-area="{escape(area)}">{escape(area)} ({count})</button>'
        )

    cards: list[str] = []
    for story in stories:
        sid = escape(str(story["id"]))
        area = escape(str(story["area"]))
        title = escape(str(story["title"]))
        desc = escape(str(story.get("description", "")))
        font_scale = float(story.get("fontScale", 1.0))
        scale_badge = (
            f'<span class="badge">{font_scale:.1f}x text</span>'
            if abs(font_scale - 1.0) > 0.01
            else ""
        )
        light_src = escape(str(story["lightPng"]))
        dark_src = escape(str(story["darkPng"]))
        cards.append(
            f"""
            <article class="story" id="{sid}" data-area="{area}" data-id="{sid}" data-search="{sid.lower()} {title.lower()} {desc.lower()}">
              <header class="story-header">
                <div>
                  <div class="story-meta">
                    <span class="story-id">{sid}</span>
                    <span class="badge">{area}</span>
                    {scale_badge}
                  </div>
                  <h2>{title}</h2>
                  <p>{desc}</p>
                </div>
              </header>
              <div class="shots">
                <figure class="shot">
                  <figcaption>Light</figcaption>
                  <img src="{light_src}" alt="{title} (Light)" loading="lazy" />
                </figure>
                <figure class="shot shot-dark">
                  <figcaption>Dark</figcaption>
                  <img src="{dark_src}" alt="{title} (Dark)" loading="lazy" />
                </figure>
              </div>
              {render_flags(flags_by_story[str(story["id"])])}
            </article>
            """
        )

    return f"""<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>amux Android Storybook</title>
  <style>
    :root {{
      --bg: #f6f6f5;
      --surface: #ffffff;
      --sunken: #eeeeec;
      --hairline: #dcdcd9;
      --ink: #161615;
      --muted: #5c5c58;
      --accent: #d95d39;
    }}
    * {{ box-sizing: border-box; }}
    body {{
      margin: 0;
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
      background: var(--bg);
      color: var(--ink);
    }}
    header.top {{
      position: sticky;
      top: 0;
      z-index: 10;
      background: var(--surface);
      border-bottom: 1px solid var(--hairline);
      padding: 16px 24px;
      display: flex;
      flex-wrap: wrap;
      gap: 12px 20px;
      align-items: center;
      justify-content: space-between;
    }}
    header.top h1 {{
      margin: 0;
      font-size: 18px;
      font-weight: 600;
      letter-spacing: -0.01em;
    }}
    .controls {{
      display: flex;
      flex-wrap: wrap;
      gap: 8px;
      align-items: center;
    }}
    .tabs {{
      display: flex;
      flex-wrap: wrap;
      gap: 6px;
    }}
    .tab {{
      appearance: none;
      border: 1px solid var(--hairline);
      background: var(--surface);
      color: var(--muted);
      border-radius: 8px;
      padding: 6px 12px;
      font-size: 13px;
      font-weight: 500;
      cursor: pointer;
    }}
    .tab.active {{
      background: var(--ink);
      border-color: var(--ink);
      color: #ffffff;
    }}
    #search {{
      border: 1px solid var(--hairline);
      border-radius: 8px;
      padding: 6px 10px;
      font-size: 13px;
      min-width: 200px;
      background: var(--bg);
      color: var(--ink);
    }}
    main {{
      max-width: 1120px;
      margin: 0 auto;
      padding: 24px;
      display: flex;
      flex-direction: column;
      gap: 24px;
    }}
    .story {{
      background: var(--surface);
      border: 1px solid var(--hairline);
      border-radius: 14px;
      padding: 20px;
    }}
    .story.hidden {{ display: none; }}
    .story-meta {{
      display: flex;
      flex-wrap: wrap;
      gap: 8px;
      align-items: center;
      margin-bottom: 6px;
    }}
    .story-id {{
      font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
      font-size: 12px;
      color: var(--muted);
    }}
    .badge {{
      font-size: 11px;
      font-weight: 600;
      padding: 2px 8px;
      border-radius: 999px;
      background: var(--sunken);
      color: var(--muted);
    }}
    .story-header h2 {{
      margin: 0 0 4px 0;
      font-size: 17px;
      font-weight: 600;
    }}
    .story-header p {{
      margin: 0 0 16px 0;
      font-size: 13px;
      color: var(--muted);
    }}
    .shots {{
      display: grid;
      grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
      gap: 16px;
    }}
    .shot {{
      margin: 0;
      background: var(--bg);
      border: 1px solid var(--hairline);
      border-radius: 12px;
      padding: 12px;
      display: flex;
      flex-direction: column;
      align-items: center;
      gap: 8px;
    }}
    .shot.shot-dark {{
      background: #141413;
      border-color: #2c2c29;
      color: #a1a19a;
    }}
    .shot figcaption {{
      font-size: 12px;
      font-weight: 500;
      align-self: flex-start;
    }}
    .geometry, .geometry-none {{
      margin-top: 12px;
      font-size: 13px;
      color: var(--muted);
    }}
    .geometry summary {{ cursor: pointer; font-weight: 500; }}
    .geometry ul {{ margin: 8px 0 0 0; padding-left: 18px; }}
    .geometry li {{ margin: 2px 0; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; }}
    .geometry .rule {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; font-weight: 600; color: var(--ink); }}
    .geometry-summary {{
      background: var(--surface);
      border: 1px solid var(--hairline);
      border-radius: 14px;
      padding: 20px;
    }}
    .geometry-summary h2 {{ margin: 0 0 4px 0; font-size: 17px; font-weight: 600; }}
    .geometry-summary p {{ margin: 0 0 12px 0; font-size: 13px; color: var(--muted); }}
    .geometry-summary .tables {{ display: flex; flex-wrap: wrap; gap: 24px; }}
    .geometry-summary table {{ border-collapse: collapse; font-size: 13px; }}
    .geometry-summary td, .geometry-summary th {{ text-align: left; padding: 4px 12px 4px 0; border-bottom: 1px solid var(--hairline); }}
    .shot img {{
      width: 100%;
      max-width: 340px;
      height: auto;
      border-radius: 10px;
      border: 1px solid rgba(128, 128, 128, 0.25);
      display: block;
    }}
  </style>
</head>
<body>
  <header class="top">
    <h1>amux Android Storybook</h1>
    <div class="controls">
      <div class="tabs" role="tablist">
        {"".join(tab_buttons)}
      </div>
      <input id="search" type="search" placeholder="Filter stories…" aria-label="Filter stories" />
    </div>
  </header>
  <main>
    {render_geometry_summary(flags_by_story)}
    {"".join(cards)}
  </main>
  <script>
    (function() {{
      let activeArea = "All";
      const tabs = document.querySelectorAll(".tab");
      const search = document.getElementById("search");
      const stories = document.querySelectorAll(".story");
      function apply() {{
        const q = (search.value || "").trim().toLowerCase();
        stories.forEach((el) => {{
          const areaOk = activeArea === "All" || el.dataset.area === activeArea;
          const queryOk = !q || (el.dataset.search || "").includes(q);
          el.classList.toggle("hidden", !(areaOk && queryOk));
        }});
      }}
      tabs.forEach((btn) => {{
        btn.addEventListener("click", () => {{
          activeArea = btn.dataset.area;
          tabs.forEach((b) => b.classList.toggle("active", b === btn));
          apply();
        }});
      }});
      search.addEventListener("input", apply);
    }})();
  </script>
</body>
</html>
"""


def write_gallery(out_dir: Path) -> tuple[Path, list[dict]]:
    stories = load_manifest(out_dir)
    index_path = out_dir / "index.html"
    index_path.write_text(render_html(stories, out_dir), encoding="utf-8")
    return index_path, stories


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    out_dir = args.out.resolve()
    out_dir.mkdir(parents=True, exist_ok=True)

    if not args.html_only:
        cmd = build_gradle_command(
            out_dir=out_dir,
            filters=args.filters,
            no_daemon=args.no_daemon,
        )
        completed = subprocess.run(cmd, cwd=REPO_ROOT)
        if completed.returncode != 0:
            return completed.returncode

    index_path, stories = write_gallery(out_dir)
    print(
        f"storybook: rendered {len(stories)} stories ({len(stories) * 2} PNGs) -> {index_path}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
