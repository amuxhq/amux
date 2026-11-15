// The overlay's look. It sits in a shadow root, so nothing here reaches the
// page and nothing on the page reaches it. The palette and the controls are
// the ones the extension's panel uses (tokens.ts); this adds what only the
// overlay draws. Surfaces are opaque: the page under a comment box must not
// show through it.

import { CONTROLS, tokens } from "./tokens.ts";

export const STYLE = `${tokens(":host")}
:host {
  all: initial;
  position: fixed;
  inset: 0;
  z-index: 2147483647;
  pointer-events: none;
  font: var(--font);
  color: var(--text);
}
[hidden] { display: none !important; }
${CONTROLS}
.hover, .box {
  position: fixed;
  box-sizing: border-box;
  border: 2px solid var(--accent);
  background: var(--accent-wash);
  border-radius: 4px;
  pointer-events: none;
}
.box { border-style: dashed; }
/* On the block's top right: its top left is where a pin points. */
.hover-label {
  position: absolute;
  right: -2px;
  bottom: 100%;
  margin-bottom: 4px;
  max-width: 360px;
  padding: 2px 7px;
  border-radius: 5px;
  background: var(--accent);
  color: var(--accent-text);
  font-size: 11px;
  font-weight: 500;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.hover-label:empty { display: none; }
.pins { position: fixed; inset: 0; pointer-events: none; }
.pin {
  position: fixed;
  width: 22px;
  height: 22px;
  padding: 0;
  border: 2px solid white;
  border-radius: 11px 11px 11px 2px;
  background: var(--accent);
  color: white;
  font: 600 11px/18px -apple-system, BlinkMacSystemFont, system-ui, sans-serif;
  text-align: center;
  box-shadow: 0 2px 6px rgba(0, 0, 0, 0.25);
  cursor: pointer;
  pointer-events: auto;
  transition: transform 0.12s ease;
  transform-origin: bottom left;
}
.pin:hover, .pin.current { transform: scale(1.15); }
.pin.current { box-shadow: var(--ring), 0 2px 6px rgba(0, 0, 0, 0.25); }
.editor {
  position: fixed;
  width: 340px;
  box-sizing: border-box;
  padding: 10px;
  border-radius: 12px;
  background: var(--surface);
  box-shadow: var(--shadow-pop);
  pointer-events: auto;
}
.editor-target {
  margin: 0 2px 8px;
  color: var(--muted);
  font-size: 12px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.editor-row { display: flex; align-items: center; gap: 4px; margin-top: 10px; }
.grow { flex: 1; }
.toolbar {
  position: fixed;
  left: 50%;
  bottom: 20px;
  transform: translateX(-50%);
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 6px 6px 12px;
  border-radius: 12px;
  background: var(--surface);
  box-shadow: var(--shadow-pop);
  pointer-events: auto;
  white-space: nowrap;
}
.mark { width: 8px; height: 8px; border-radius: 50%; background: var(--accent); box-shadow: 0 0 0 3px var(--accent-wash); }
.hint { color: var(--muted); margin-right: 2px; }
.count { font-weight: 500; padding: 0 4px 0 10px; border-left: 1px solid var(--border); }
.count:empty { display: none; }
.tool-box[aria-pressed="true"] { background: var(--accent-wash); color: var(--accent); }
`;
