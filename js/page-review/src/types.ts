// What the overlay and whoever loads it exchange. The overlay knows nothing
// of amux, agents, profiles or transport: a Chrome extension loads it into a
// tab, an app loads it into its web view, and each is a host. Every host
// ships the overlay inside its own build, so this interface is never a
// promise between versions.

/** The block a comment is anchored on, as an agent can search for it. */
export interface Anchor {
  /** A CSS selector for the block when the comment was made. */
  selector: string;
  /** The block's visible text, cut short. */
  text: string;
  /** Where the page says the block was drawn from, "path:line"; empty when it does not say. */
  source: string;
}

/** A dragged box, in CSS pixels from the viewport's top left. */
export interface Region {
  x: number;
  y: number;
  width: number;
  height: number;
  /** Device pixels per CSS pixel, for cropping a screenshot. */
  devicePixelRatio: number;
}

/** A comment as the overlay hands it over. */
export interface Comment {
  id: string;
  element: Anchor;
  /** The selected phrase within the block; empty for the whole block. */
  quote: string;
  /** Set for a dragged box: the host captured it as a screenshot. */
  region: Region | null;
  text: string;
  /** Where to draw its pin, in page coordinates. */
  pin: { x: number; y: number };
}

/** What a host does for the overlay. */
export interface Host {
  /** A comment was added. */
  added(comment: Comment): void;
  /** A comment's words changed. */
  edited(id: string, text: string): void;
  deleted(id: string): void;
  /**
   * Capture `region` of what is on screen now, for comment `id`, and keep it
   * with the comment. The overlay hides itself until this settles. Answers
   * whether a picture was taken.
   */
  capture(id: string, region: Region): Promise<boolean>;
  /** The person left review mode. */
  stopped(): void;
  /**
   * Where the page's own code says an element was drawn from, when the host
   * can ask the page (React keeps it where only the page's scripts read it).
   */
  sourceOf?(element: Element): Promise<string>;
}

/** The overlay, as a host drives it. */
export interface Overlay {
  start(): void;
  stop(): void;
  readonly active: boolean;
  /** The comments to draw as numbered pins, in order. */
  setComments(comments: readonly Comment[]): void;
  /** Scroll to a comment's pin and open it. */
  focus(id: string): void;
  destroy(): void;
}
