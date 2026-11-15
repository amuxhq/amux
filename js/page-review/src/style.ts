// The overlay's look. It sits in a shadow root, so nothing here reaches the
// page and nothing on the page reaches it. System type, quiet chrome, one
// accent for what is targeted. Surfaces are opaque: the page under a comment
// box must not show through it.

export const STYLE = `
:host {
  all: initial;
  position: fixed;
  inset: 0;
  z-index: 2147483647;
  pointer-events: none;
  font: 13px/1.4 -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif;
  color-scheme: light dark;
  --accent: #0a84ff;
  --accent-wash: rgba(10, 132, 255, 0.08);
  --surface: #ffffff;
  --raised: #f5f5f7;
  --text: #1d1d1f;
  --muted: #6e6e73;
  --line: rgba(0, 0, 0, 0.12);
  --shadow: 0 12px 32px rgba(0, 0, 0, 0.16), 0 2px 6px rgba(0, 0, 0, 0.10), 0 0 0 0.5px rgba(0, 0, 0, 0.12);
}
@media (prefers-color-scheme: dark) {
  :host {
    --surface: #2a2a2c;
    --raised: #3a3a3c;
    --text: #f5f5f7;
    --muted: #a1a1a6;
    --line: rgba(255, 255, 255, 0.14);
    --shadow: 0 12px 32px rgba(0, 0, 0, 0.45), 0 2px 6px rgba(0, 0, 0, 0.3), 0 0 0 0.5px rgba(255, 255, 255, 0.12);
  }
}
[hidden] { display: none !important; }
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
  color: white;
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
.pin.current { box-shadow: 0 0 0 3px rgba(10, 132, 255, 0.35), 0 2px 6px rgba(0, 0, 0, 0.25); }
.editor {
  position: fixed;
  width: 340px;
  box-sizing: border-box;
  padding: 10px;
  border-radius: 12px;
  background: var(--surface);
  color: var(--text);
  box-shadow: var(--shadow);
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
textarea {
  display: block;
  width: 100%;
  min-height: 64px;
  box-sizing: border-box;
  resize: vertical;
  padding: 7px 9px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--surface);
  color: inherit;
  font: inherit;
  outline: none;
}
textarea::placeholder { color: var(--muted); }
textarea:focus { border-color: var(--accent); box-shadow: 0 0 0 3px rgba(10, 132, 255, 0.18); }
.editor-row, .toolbar { display: flex; align-items: center; gap: 6px; }
.editor-row { margin-top: 10px; }
.grow { flex: 1; }
button {
  font: inherit;
  font-weight: 500;
  color: inherit;
  padding: 4px 11px;
  border: 0;
  border-radius: 7px;
  background: var(--raised);
  cursor: pointer;
}
button:hover { filter: brightness(0.96); }
.save, .done { background: var(--accent); color: white; }
.save:hover, .done:hover { filter: brightness(1.08); }
.delete { background: transparent; color: #ff453a; padding-left: 6px; padding-right: 6px; }
.wider { background: transparent; color: var(--muted); padding-left: 6px; padding-right: 6px; }
.wider:hover, .delete:hover { background: var(--raised); filter: none; }
.toolbar {
  position: fixed;
  left: 50%;
  bottom: 20px;
  transform: translateX(-50%);
  padding: 6px 6px 6px 12px;
  border-radius: 12px;
  background: var(--surface);
  color: var(--text);
  box-shadow: var(--shadow);
  pointer-events: auto;
  white-space: nowrap;
  gap: 10px;
}
.mark { width: 8px; height: 8px; border-radius: 50%; background: var(--accent); box-shadow: 0 0 0 3px rgba(10, 132, 255, 0.2); }
.hint { color: var(--muted); }
.count { color: var(--text); font-weight: 500; padding-left: 10px; border-left: 1px solid var(--line); }
.count:empty { display: none; }
.tool-box[aria-pressed="true"] { background: var(--accent-wash); color: var(--accent); box-shadow: inset 0 0 0 1px var(--accent); }
`;
