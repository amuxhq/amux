// Builds the extension into dist/, ready to load unpacked or zip for the
// store: the four scripts, the panel's page, the manifest, the icon, and
// browser-core's WebAssembly, which `just browser` builds first.
//
// `--dev` builds the copy a development tree loads: it names itself
// "amux (dev)" and starts the `sh.amux.browser.dev` host, which
// `just browser-dev-host` points at that tree's own amux, so it never
// talks to an installed one.

import { build } from "esbuild";
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { CONTROLS, tokens } from "../page-review/src/tokens.ts";

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, "../..");
const dev = process.argv.includes("--dev");
const out = join(here, dev ? "dist-dev" : "dist");
const wasm =
  process.env.AMUX_BROWSER_CORE ?? join(repo, "target/wasm32-unknown-unknown/wasm/browser_core.wasm");

if (!existsSync(wasm)) {
  console.error(`No ${wasm}: build it first with \`just browser\`.`);
  process.exit(1);
}

rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });

const common = {
  bundle: true,
  target: "chrome116",
  logLevel: "warning" as const,
  minify: !dev,
  sourcemap: dev ? ("inline" as const) : false,
  define: { __AMUX_HOST__: JSON.stringify(dev ? "sh.amux.browser.dev" : "sh.amux.browser") },
  jsx: "automatic" as const,
  jsxImportSource: "preact",
};

await Promise.all([
  build({ ...common, entryPoints: [join(here, "src/background/index.ts")], format: "esm", outfile: join(out, "background.js") }),
  build({ ...common, entryPoints: [join(here, "src/content/index.ts")], format: "iife", outfile: join(out, "content.js") }),
  build({ ...common, entryPoints: [join(here, "src/page/index.ts")], format: "iife", outfile: join(out, "page.js") }),
  build({ ...common, entryPoints: [join(here, "src/panel/index.tsx")], format: "esm", outfile: join(out, "panel.js") }),
]);

const manifest = JSON.parse(readFileSync(join(here, "static/manifest.json"), "utf8"));
if (dev) manifest.name = "amux (dev)";
writeFileSync(join(out, "manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
copyFileSync(join(here, "static/panel.html"), join(out, "panel.html"));
// The panel's sheet starts with the palette and controls every amux surface
// on the web shares, the overlay's included.
writeFileSync(
  join(out, "panel.css"),
  `${tokens(":root")}\n${CONTROLS}\n${readFileSync(join(here, "static/panel.css"), "utf8")}`,
);
copyFileSync(join(repo, "images/brand/amux.png"), join(out, "icon.png"));
copyFileSync(wasm, join(out, "browser_core.wasm"));
console.log(`built ${out}`);
