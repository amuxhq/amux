# Logo source and exports

The editable master is [amux.svg](../images/brand/amux.svg). It uses ordinary
SVG paths, gradients and a clipping path; it contains no raster image, font,
filter, or external dependency. Open it in a vector editor or edit the XML.

The rounded A identifies amux. A paper plane cut through it signals message
passing, and a sweeping flight trail supplies the crossbar. The cutout has
rounded tips and flows directly into the open gap between the legs. The
lower-left ribbon is disconnected from the rest of the mark, with a smooth,
tapered inner edge. Colour supplies ribbon depth; the silhouette also works
in a single colour.

For a place that takes no SVG, [amux.png](../images/brand/amux.png),
[amux-black.png](../images/brand/amux-black.png) and
[amux-white.png](../images/brand/amux-white.png) are the colour, black and
white exports at 1024 pixels, on a transparent ground. `just logo` makes them
again, with every other size, whenever the master changes.

## Editing

`silhouette` owns the outline and all negative space. The other named paths
and gradients provide colour inside that outline. `crown` is the upper fold,
`front-fold` is the warm right foot, and `left-foot` keeps the lower left
blue. The plane and flight trail are transparent, including on dark surfaces.

The square viewBox includes clear space. Exports preserve that framing at
every size. Keep the SVG as the master instead of tracing a PNG export.

## Export and review

Install `uv`, then run from the repository root:

```sh
just logo
```

The recipe uses pinned renderer and Pillow versions, with a 120-second
timeout. The first run downloads its Python dependencies. It writes to
`target/brand/` by default:

- A copy of the SVG master and `amux-mono.svg`, whose fill is `currentColor`,
  plus standalone `amux-black.svg` and `amux-white.svg` versions.
- Transparent colour, black and white PNGs at 16, 24, 32, 48, 64, 128, 256,
  512, 1024 and 2048 pixels.
- `review.png`, showing the mark on light and dark backgrounds, the monochrome
  silhouette, and small exports at their actual pixel sizes.
- `monochrome-review.png`, showing large black and white versions and both
  versions at actual sizes from 16 to 128 pixels.
- `app-icon.png`, `app-icon-dark.png` and `app-icon-tinted.png`, the iPhone
  app's icon on black, on a transparent ground for the dark home screen, and
  as the white silhouette for the tinted one. The mark is drawn at 80% of the
  canvas so the rounded corners clear it. Copy them over the three images in
  `apps/apple/Amux/Assets.xcassets/AppIcon.appiconset/`.
- `checks.json`, reporting dimensions, bounds, transparency probes and the
  two disconnected ribbon shapes.

Use `--out DIRECTORY` to choose another output location. To compare against
the original 1254×1254 concept image, add `--reference /path/to/reference.png`.
The comparison crops the reference to the master's viewBox, so both marks
have the same scale. The reference is optional and is not needed for export.
A reference run also writes `comparison.png`, a larger side-by-side view.

For each revision:

1. Export to a separate output directory to retain the preceding review.
2. Inspect the monochrome version first: rounded tips, continuous flight
   trail, open gap between the legs, and a smooth inner edge on the left leg.
3. Inspect the colour version for unwanted seams, clipped gradients or folds
   that obscure the plane. Confirm that light and dark backgrounds show
   through the cutout.
4. Inspect the actual-size samples. At 16 pixels the plane detail is tiny;
   assess the overall silhouette there. Use 32 pixels or more when the
   cutout's detail matters.
5. Change the geometry or shading responsible for the problem, re-export,
   and compare the same views. Preserve the reference's flowing geometry;
   small differences in gradient shading are acceptable.

`currentColor` inherits when the monochrome SVG is inline; when loaded as a
standalone image it defaults to black. Use the explicit black or white export
when an image must have that colour regardless of its host. PNGs retain alpha;
compose a background when a destination requires an opaque image.
