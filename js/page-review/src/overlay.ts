// Review mode: the hover outline, the three gestures, the comment box and
// the pins, drawn in a shadow root so the page's styles and ours never meet.
//
// While review mode is on, the page does not see clicks: a click comments on
// the block under it. Selecting text comments on the phrase; dragging from
// anywhere that is not text, or with the box tool on, draws a box, which is
// captured as a screenshot. Holding Shift hands the pointer back to the page
// for as long as it is held. Escape closes the comment box, then leaves.
//
// Gestures are built on pointer events, with no assumption of a mouse, so a
// touch mode (which has no hover) can be added rather than written anew.

import { anchorOf, blockAround, label, pointable, snap, wider } from "./blocks.ts";
import { STYLE } from "./style.ts";
import type { Comment, Host, Overlay, Region } from "./types.ts";

/** A drag shorter than this either way is a click. */
const DRAG = 6;

type Target =
  | { kind: "block"; block: Element }
  | { kind: "quote"; block: Element; quote: string; rect: DOMRect }
  | { kind: "region"; block: Element; region: Region };

interface Editing {
  /** Set when editing a comment already made. */
  id: string | null;
  target: Target | null;
  text: string;
}

export function createOverlay(host: Host): Overlay {
  const root = document.createElement("amux-page-review");
  root.setAttribute("data-amux", "");
  const shadow = root.attachShadow({ mode: "open" });
  shadow.innerHTML = `<style>${STYLE}</style>
    <div class="hover" hidden><div class="hover-label"></div></div>
    <div class="box" hidden></div>
    <div class="pins"></div>
    <form class="editor" hidden>
      <div class="editor-target"></div>
      <textarea rows="3" placeholder="Add a comment"></textarea>
      <div class="editor-row">
        <button type="button" class="wider" title="Comment on the block around it">Wider</button>
        <button type="button" class="delete">Delete</button>
        <span class="grow"></span>
        <button type="button" class="cancel">Cancel</button>
        <button type="submit" class="save" title="⌘↩">Add</button>
      </div>
    </form>
    <div class="toolbar" hidden>
      <span class="mark" aria-hidden="true"></span>
      <span class="hint">Click a block, select text, or drag a box</span>
      <button type="button" class="tool-box" aria-pressed="false" title="Drag a box over anything, text included">Box</button>
      <span class="count"></span>
      <button type="button" class="done">Done</button>
    </div>`;
  const $ = <T extends Element>(selector: string) => shadow.querySelector(selector) as T;
  const hover = $<HTMLDivElement>(".hover");
  const hoverLabel = $<HTMLDivElement>(".hover-label");
  const box = $<HTMLDivElement>(".box");
  const pins = $<HTMLDivElement>(".pins");
  const editor = $<HTMLFormElement>(".editor");
  const editorTarget = $<HTMLDivElement>(".editor-target");
  const textarea = $<HTMLTextAreaElement>("textarea");
  const widerButton = $<HTMLButtonElement>(".wider");
  const deleteButton = $<HTMLButtonElement>(".delete");
  const saveButton = $<HTMLButtonElement>(".save");
  const toolbar = $<HTMLDivElement>(".toolbar");
  const toolBox = $<HTMLButtonElement>(".tool-box");
  const count = $<HTMLSpanElement>(".count");

  let active = false;
  let passThrough = false;
  let boxTool = false;
  let comments: readonly Comment[] = [];
  let editing: Editing | null = null;
  let drag: { x: number; y: number; box: boolean; moved: boolean } | null = null;
  let frame = 0;

  const ours = (event: Event) => event.composedPath().includes(root);

  function place(element: HTMLElement, rect: { left: number; top: number; width: number; height: number }) {
    element.style.left = `${rect.left}px`;
    element.style.top = `${rect.top}px`;
    element.style.width = `${rect.width}px`;
    element.style.height = `${rect.height}px`;
  }

  function showHover(block: Element | null) {
    if (!block || !pointable(block) || editing || passThrough) {
      hover.hidden = true;
      return;
    }
    place(hover, block.getBoundingClientRect());
    hoverLabel.textContent = label(block);
    hover.hidden = false;
  }

  function regionOf(x1: number, y1: number, x2: number, y2: number): Region {
    return {
      x: Math.min(x1, x2),
      y: Math.min(y1, y2),
      width: Math.abs(x2 - x1),
      height: Math.abs(y2 - y1),
      devicePixelRatio: window.devicePixelRatio || 1,
    };
  }

  function targetRect(target: Target): DOMRect {
    switch (target.kind) {
      case "block":
        return target.block.getBoundingClientRect();
      case "quote":
        return target.rect;
      case "region":
        return new DOMRect(target.region.x, target.region.y, target.region.width, target.region.height);
    }
  }

  function describe(target: Target): string {
    switch (target.kind) {
      case "block":
        return label(target.block);
      case "quote":
        return `“${target.quote.length > 60 ? `${target.quote.slice(0, 59)}…` : target.quote}”`;
      case "region":
        return "Box";
    }
  }

  /** Opens the comment box below `rect`, outlining it when `outline` is set. */
  function openEditor(next: Editing, rect: DOMRect, outline = next.target?.kind !== "quote") {
    editing = next;
    hover.hidden = true;
    editorTarget.textContent = next.target ? describe(next.target) : "";
    textarea.value = next.text;
    widerButton.hidden = !next.target || next.target.kind === "region" || !wider(next.target.block);
    deleteButton.hidden = next.id === null;
    saveButton.textContent = next.id === null ? "Add" : "Save";
    editor.hidden = false;
    // Below the target when it fits, else above it; never off screen.
    const height = editor.offsetHeight || 140;
    const width = editor.offsetWidth || 320;
    const below = rect.bottom + 8;
    const top = below + height < window.innerHeight ? below : Math.max(8, rect.top - height - 8);
    const left = Math.min(Math.max(8, rect.left), window.innerWidth - width - 8);
    editor.style.left = `${left}px`;
    editor.style.top = `${top}px`;
    if (outline && rect.width > 0) {
      place(box, rect);
      box.hidden = false;
    }
    textarea.focus();
  }

  function closeEditor() {
    const wasEditing = editing?.id ?? null;
    editing = null;
    editor.hidden = true;
    box.hidden = true;
    if (wasEditing) drawPins();
  }

  async function save() {
    if (!editing) return;
    const text = textarea.value.trim();
    const { id, target } = editing;
    closeEditor();
    if (id !== null) {
      host.edited(id, text);
      return;
    }
    if (!target) return;
    const source = (await host.sourceOf?.(target.block).catch(() => "")) ?? "";
    const rect = targetRect(target);
    // A block's pin sits on its corner; a phrase's at its end, where the
    // reading stops; a box's on its corner.
    const corner = target.kind === "quote" ? { x: rect.right, y: rect.top } : { x: rect.left, y: rect.top };
    const comment: Comment = {
      id: pendingId ?? crypto.randomUUID(),
      element: anchorOf(target.block, source),
      quote: target.kind === "quote" ? target.quote : "",
      region: target.kind === "region" ? target.region : null,
      text,
      pin: { x: corner.x + window.scrollX, y: corner.y + window.scrollY },
    };
    pendingId = null;
    window.getSelection()?.removeAllRanges();
    host.added(comment);
  }

  /** The id a box's screenshot was captured under, before its comment is added. */
  let pendingId: string | null = null;

  async function capture(region: Region, block: Element) {
    const id = crypto.randomUUID();
    root.style.visibility = "hidden";
    // Two frames: the hidden overlay has been painted away before the shot.
    await new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)));
    let taken = false;
    try {
      taken = await host.capture(id, region);
    } finally {
      root.style.visibility = "";
    }
    pendingId = taken ? id : null;
    const target: Target = { kind: "region", block, region };
    openEditor({ id: null, target, text: "" }, targetRect(target));
  }

  function overText(x: number, y: number): boolean {
    const range = document.caretRangeFromPoint?.(x, y);
    const node = range?.startContainer;
    if (!node || node.nodeType !== Node.TEXT_NODE || !node.textContent?.trim()) return false;
    const whole = document.createRange();
    whole.selectNodeContents(node);
    return Array.from(whole.getClientRects()).some(
      (rect) => x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom,
    );
  }

  function elementAt(x: number, y: number): Element | null {
    root.style.pointerEvents = "none";
    const element = document.elementFromPoint(x, y);
    root.style.pointerEvents = "";
    return element && element !== root ? element : null;
  }

  const listeners: Array<[string, (event: Event) => void]> = [
    [
      "pointermove",
      (event) => {
        const pointer = event as PointerEvent;
        if (drag) {
          if (Math.abs(pointer.clientX - drag.x) > DRAG || Math.abs(pointer.clientY - drag.y) > DRAG) {
            drag.moved = true;
          }
          if (drag.box && drag.moved) {
            const region = regionOf(drag.x, drag.y, pointer.clientX, pointer.clientY);
            place(box, { left: region.x, top: region.y, width: region.width, height: region.height });
            box.hidden = false;
            hover.hidden = true;
          }
          return;
        }
        if (ours(event)) {
          // A pin outlines its own target while pointed at; the rest of ours hides the outline.
          const onPin = event.composedPath().some((node) => node instanceof HTMLElement && node.classList.contains("pin"));
          if (!onPin) hover.hidden = true;
          return;
        }
        if (passThrough || pointer.shiftKey) {
          hover.hidden = true;
          return;
        }
        const element = elementAt(pointer.clientX, pointer.clientY);
        showHover(element ? snap(element) : null);
      },
    ],
    [
      "pointerdown",
      (event) => {
        const pointer = event as PointerEvent;
        if (ours(event) || passThrough || pointer.shiftKey || pointer.button !== 0) return;
        event.stopPropagation();
        if (editing) {
          closeEditor();
        }
        const asBox = boxTool || !overText(pointer.clientX, pointer.clientY);
        if (asBox) event.preventDefault();
        drag = { x: pointer.clientX, y: pointer.clientY, box: asBox, moved: false };
      },
    ],
    [
      "pointerup",
      (event) => {
        const pointer = event as PointerEvent;
        const ended = drag;
        drag = null;
        if (!ended) return;
        event.stopPropagation();
        if (ended.box && ended.moved) {
          box.hidden = true;
          const region = regionOf(ended.x, ended.y, pointer.clientX, pointer.clientY);
          const block = blockAround(region.x, region.y, region.width, region.height);
          if (block) void capture(region, block);
          return;
        }
        const selection = window.getSelection();
        const quote = selection && !selection.isCollapsed ? selection.toString().trim() : "";
        if (quote && selection && selection.rangeCount > 0) {
          const range = selection.getRangeAt(0);
          const common = range.commonAncestorContainer;
          const inside = common.nodeType === Node.ELEMENT_NODE ? (common as Element) : common.parentElement;
          if (inside) {
            const target: Target = { kind: "quote", block: snap(inside), quote, rect: range.getBoundingClientRect() };
            openEditor({ id: null, target, text: "" }, targetRect(target));
            return;
          }
        }
        const element = elementAt(pointer.clientX, pointer.clientY);
        const block = element ? snap(element) : null;
        if (!block || !pointable(block)) return;
        const target: Target = { kind: "block", block };
        openEditor({ id: null, target, text: "" }, targetRect(target));
      },
    ],
    // The page acts on clicks, not on the pointer events above: those are
    // the ones that would follow a link or press a button.
    ["click", (event) => blockPage(event, true)],
    ["auxclick", (event) => blockPage(event, true)],
    ["dblclick", (event) => blockPage(event, true)],
    ["mousedown", (event) => blockPage(event, false)],
    ["mouseup", (event) => blockPage(event, false)],
    [
      "keydown",
      (event) => {
        const key = event as KeyboardEvent;
        if (key.key === "Shift") {
          passThrough = true;
          hover.hidden = true;
        }
        if (key.key === "Escape") {
          event.stopPropagation();
          event.preventDefault();
          if (editing) closeEditor();
          else stop();
        }
      },
    ],
    [
      "keyup",
      (event) => {
        if ((event as KeyboardEvent).key === "Shift") passThrough = false;
      },
    ],
    ["scroll", () => schedule()],
    ["resize", () => schedule()],
  ];

  function blockPage(event: Event, prevent: boolean) {
    if (ours(event) || passThrough || (event as MouseEvent).shiftKey) return;
    event.stopPropagation();
    if (prevent) event.preventDefault();
  }

  function schedule() {
    if (frame) return;
    frame = requestAnimationFrame(() => {
      frame = 0;
      drawPins();
      if (!hover.hidden) hover.hidden = true;
    });
  }

  /**
   * Where a pin is drawn now, in the viewport. A block's follows the block,
   * so a layout that moves keeps its pins; a phrase's and a box's stay where
   * they were made.
   */
  function pinPlace(comment: Comment): { x: number; y: number } {
    if (!comment.region && !comment.quote) {
      const found = (() => {
        try {
          return document.querySelector(comment.element.selector);
        } catch {
          return null;
        }
      })();
      if (found) {
        const rect = found.getBoundingClientRect();
        return { x: rect.left, y: rect.top };
      }
    }
    return { x: comment.pin.x - window.scrollX, y: comment.pin.y - window.scrollY };
  }

  /**
   * What a comment is about, in the viewport now: its block, or its box. A
   * phrase's block stands in for the phrase, whose range is not kept.
   */
  function anchorRect(comment: Comment): DOMRect {
    const { x, y } = pinPlace(comment);
    if (comment.region) return new DOMRect(x, y, comment.region.width, comment.region.height);
    const found = (() => {
      try {
        return document.querySelector(comment.element.selector);
      } catch {
        return null;
      }
    })();
    return found ? found.getBoundingClientRect() : new DOMRect(x, y, 0, 0);
  }

  function editComment(comment: Comment) {
    const rect = anchorRect(comment);
    openEditor({ id: comment.id, target: null, text: comment.text }, rect, true);
    editorTarget.textContent = comment.quote
      ? `“${comment.quote.length > 60 ? `${comment.quote.slice(0, 59)}…` : comment.quote}”`
      : comment.region
        ? "Box"
        : comment.element.text || comment.element.selector;
  }

  function drawPins() {
    pins.replaceChildren(
      ...comments.map((comment, index) => {
        const pin = document.createElement("button");
        pin.type = "button";
        pin.className = "pin";
        pin.textContent = String(index + 1);
        pin.title = comment.text || "No words yet";
        const { x, y } = pinPlace(comment);
        // The pin's pointed corner, its lower left, touches the spot from
        // above, so it never covers what it is about.
        pin.style.left = `${Math.min(Math.max(0, x), window.innerWidth - 24)}px`;
        pin.style.top = `${Math.max(0, y - 22)}px`;
        if (editing?.id === comment.id) pin.classList.add("current");
        pin.addEventListener("click", (event) => {
          event.stopPropagation();
          editComment(comment);
          drawPins();
        });
        // Pointing at a pin outlines what it is about.
        pin.addEventListener("pointerenter", () => {
          if (editing) return;
          const rect = anchorRect(comment);
          if (rect.width === 0) return;
          place(hover, rect);
          hoverLabel.textContent = "";
          hover.hidden = false;
        });
        pin.addEventListener("pointerleave", () => {
          if (!editing) hover.hidden = true;
        });
        return pin;
      }),
    );
    count.textContent =
      comments.length === 0 ? "" : comments.length === 1 ? "1 comment" : `${comments.length} comments`;
  }

  editor.addEventListener("submit", (event) => {
    event.preventDefault();
    void save();
  });
  textarea.addEventListener("keydown", (event) => {
    event.stopPropagation();
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      void save();
    }
    if (event.key === "Escape") {
      event.preventDefault();
      closeEditor();
    }
  });
  $<HTMLButtonElement>(".cancel").addEventListener("click", () => {
    pendingId = null;
    closeEditor();
  });
  deleteButton.addEventListener("click", () => {
    if (editing?.id) host.deleted(editing.id);
    closeEditor();
  });
  widerButton.addEventListener("click", () => {
    const target = editing?.target;
    if (!editing || !target || target.kind === "region") return;
    const next = wider(target.block);
    if (!next) return;
    const widened: Target = { kind: "block", block: next };
    openEditor({ ...editing, target: widened, text: textarea.value }, targetRect(widened));
  });
  toolBox.addEventListener("click", () => {
    boxTool = !boxTool;
    toolBox.setAttribute("aria-pressed", String(boxTool));
  });
  $<HTMLButtonElement>(".done").addEventListener("click", () => stop());

  function start() {
    if (active) return;
    active = true;
    if (!root.isConnected) document.documentElement.append(root);
    for (const [type, listener] of listeners) window.addEventListener(type, listener, true);
    toolbar.hidden = false;
    root.classList.add("active");
    drawPins();
  }

  function stop() {
    if (!active) return;
    active = false;
    closeEditor();
    hover.hidden = true;
    toolbar.hidden = true;
    root.classList.remove("active");
    for (const [type, listener] of listeners) window.removeEventListener(type, listener, true);
    host.stopped();
  }

  return {
    start,
    stop,
    get active() {
      return active;
    },
    setComments(next) {
      comments = next;
      if (!root.isConnected) document.documentElement.append(root);
      drawPins();
    },
    focus(id) {
      const comment = comments.find((candidate) => candidate.id === id);
      if (!comment) return;
      window.scrollTo({ top: Math.max(0, comment.pin.y - window.innerHeight / 3), behavior: "smooth" });
      setTimeout(() => {
        editComment(comment);
        drawPins();
      }, 300);
    },
    destroy() {
      stop();
      root.remove();
    },
  };
}
