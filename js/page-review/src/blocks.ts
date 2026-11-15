// Snapping to the block a person means, and describing it.

import type { Anchor } from "./types.ts";

/** Elements that are a block whatever their CSS says. */
const BLOCK_TAGS = new Set([
  "ARTICLE",
  "ASIDE",
  "BLOCKQUOTE",
  "BUTTON",
  "CANVAS",
  "DD",
  "DETAILS",
  "DIALOG",
  "DT",
  "FIELDSET",
  "FIGCAPTION",
  "FIGURE",
  "FOOTER",
  "FORM",
  "H1",
  "H2",
  "H3",
  "H4",
  "H5",
  "H6",
  "HEADER",
  "HR",
  "IFRAME",
  "IMG",
  "INPUT",
  "LI",
  "MAIN",
  "NAV",
  "OL",
  "P",
  "PICTURE",
  "PRE",
  "SECTION",
  "SELECT",
  "SUMMARY",
  "SVG",
  "TABLE",
  "TD",
  "TEXTAREA",
  "TH",
  "TR",
  "UL",
  "VIDEO",
]);

/** Displays that make an element a block of its own. */
const BLOCK_DISPLAYS = new Set([
  "block",
  "flex",
  "grid",
  "list-item",
  "table",
  "table-cell",
  "table-row",
  "inline-block",
  "inline-flex",
  "inline-grid",
  "flow-root",
]);

/** Smaller than this either way and an element is not a block worth naming. */
const SMALLEST = 8;

function isBlock(element: Element): boolean {
  if (BLOCK_TAGS.has(element.tagName.toUpperCase())) return true;
  const display = getComputedStyle(element).display;
  return BLOCK_DISPLAYS.has(display);
}

function bigEnough(element: Element): boolean {
  const box = element.getBoundingClientRect();
  // A layout engine that lays nothing out (a test's) gives every box zero;
  // trust the element then.
  if (box.width === 0 && box.height === 0) return true;
  return box.width >= SMALLEST && box.height >= SMALLEST;
}

/**
 * The block a pointer over `target` means: the nearest element that is a
 * block of its own and big enough to point at, never the page itself. An
 * inline run (a span, a link in a sentence, emphasis) climbs to its
 * paragraph.
 */
export function snap(target: Element): Element {
  let element: Element | null = target;
  while (element && element !== document.body && element !== document.documentElement) {
    if (isBlock(element) && bigEnough(element)) return element;
    element = element.parentElement;
  }
  return target;
}

/**
 * Whether a block is one a person points at, rather than the page around
 * everything: never the body, and nothing covering most of the window.
 */
export function pointable(block: Element): boolean {
  if (block === document.body || block === document.documentElement) return false;
  const box = block.getBoundingClientRect();
  if (box.width === 0 && box.height === 0) return true;
  const visible =
    Math.max(0, Math.min(box.right, window.innerWidth) - Math.max(box.left, 0)) *
    Math.max(0, Math.min(box.bottom, window.innerHeight) - Math.max(box.top, 0));
  return visible < 0.7 * window.innerWidth * window.innerHeight;
}

/** The block around `block`: what "wider" moves to. */
export function wider(block: Element): Element | null {
  const parent = block.parentElement;
  if (!parent || parent === document.body || parent === document.documentElement) return null;
  return snap(parent);
}

/** The smallest block that holds every corner of a box, in viewport coordinates. */
export function blockAround(x: number, y: number, width: number, height: number): Element | null {
  const corners: Array<[number, number]> = [
    [x + 1, y + 1],
    [x + width - 1, y + 1],
    [x + 1, y + height - 1],
    [x + width - 1, y + height - 1],
  ];
  let candidate = document.elementFromPoint(x + width / 2, y + height / 2);
  while (candidate && candidate !== document.documentElement) {
    const box = candidate.getBoundingClientRect();
    const holds = corners.every(([cx, cy]) => cx >= box.left && cx <= box.right && cy >= box.top && cy <= box.bottom);
    if (holds && candidate !== document.body) return snap(candidate);
    candidate = candidate.parentElement;
  }
  return document.querySelector("main") ?? document.body;
}

/** A selector that finds `element` and only it, as short as is reliable. */
export function selectorFor(element: Element): string {
  const parts: string[] = [];
  let current: Element | null = element;
  while (current && current !== document.documentElement) {
    if (current.id && document.querySelectorAll(`#${CSS.escape(current.id)}`).length === 1) {
      parts.unshift(`#${CSS.escape(current.id)}`);
      break;
    }
    const tag = current.tagName.toLowerCase();
    const parent: Element | null = current.parentElement;
    if (!parent) {
      parts.unshift(tag);
      break;
    }
    const same = Array.from(parent.children).filter((child) => child.tagName === current!.tagName);
    parts.unshift(same.length > 1 ? `${tag}:nth-of-type(${same.indexOf(current) + 1})` : tag);
    if (current === document.body) break;
    current = parent;
  }
  return parts.join(" > ");
}

/** How much of a block's text an anchor keeps. */
const MOST_TEXT = 200;

/** The block's visible text, its whitespace folded, cut short. */
export function visibleText(element: Element): string {
  const raw =
    // biome-ignore lint/performance/useDomNodeTextContent: innerText is the text as shown, without hidden parts and with layout's line breaks
    element instanceof HTMLElement ? element.innerText || element.textContent || "" : element.textContent || "";
  const folded = raw.replace(/\s+/g, " ").trim();
  if (folded.length <= MOST_TEXT) {
    if (folded) return folded;
    const label = element.getAttribute("aria-label") ?? element.getAttribute("alt") ?? "";
    return label.trim();
  }
  return `${folded.slice(0, MOST_TEXT - 1).trimEnd()}…`;
}

/**
 * Where the page's markup says an element was drawn from, by the attributes
 * development tools add, on it or its nearest marked ancestor.
 */
export function sourceFromMarkup(element: Element): string {
  let current: Element | null = element;
  while (current) {
    const path = current.getAttribute("data-inspector-relative-path") ?? current.getAttribute("data-source-file");
    if (path) {
      const line = current.getAttribute("data-inspector-line") ?? current.getAttribute("data-source-line");
      return line ? `${path}:${line}` : path;
    }
    const joined = current.getAttribute("data-source") ?? current.getAttribute("data-loc");
    if (joined && /:\d+/.test(joined)) return joined.replace(/:(\d+):\d+$/, ":$1");
    current = current.parentElement;
  }
  return "";
}

/** The anchor for a block, with the source the host found, if any. */
export function anchorOf(block: Element, source = ""): Anchor {
  return {
    selector: selectorFor(block),
    text: visibleText(block),
    source: source || sourceFromMarkup(block),
  };
}

/** A short name for a block, for the hover label: its tag and the start of its text. */
export function label(block: Element): string {
  const text = visibleText(block);
  const tag = block.tagName.toLowerCase();
  if (!text) return tag;
  return `${tag} · ${text.length > 40 ? `${text.slice(0, 39).trimEnd()}…` : text}`;
}
