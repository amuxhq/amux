// The page-review overlay: comment on blocks, phrases and regions of a page.
// A host (the Chrome extension, an app's web view) loads it and owns
// everything outside the page: screenshots, where pending comments are kept,
// and where they go.

export { createOverlay } from "./overlay.ts";
export { anchorOf, blockAround, selectorFor, snap, sourceFromMarkup, visibleText, wider } from "./blocks.ts";
export type { Anchor, Comment, Host, Overlay, Region } from "./types.ts";
