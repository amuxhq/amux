// Builds the overlay as one file an app's web view injects: it sets
// `globalThis.amuxPageReview` to the module, whose `createOverlay` the app
// calls with its host.
import { build } from "esbuild";

await build({
  entryPoints: ["src/index.ts"],
  bundle: true,
  format: "iife",
  globalName: "amuxPageReview",
  target: "es2022",
  minify: true,
  outfile: "dist/page-review.js",
  logLevel: "warning",
});
