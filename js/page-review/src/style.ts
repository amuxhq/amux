// The overlay's look. It sits in a shadow root, so nothing here reaches the
// page and nothing on the page reaches it. System type, quiet chrome, one
// accent for what is targeted.

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
  --surface: rgba(255, 255, 255, 0.98);
  --text: #1d1d1f;
  --muted: #6e6e73;
  --line: rgba(0, 0, 0, 0.12);
  --shadow: 0 8px 28px rgba(0, 0, 0, 0.18), 0 1px 3px rgba(0, 0, 0, 0.12);
}
@media (prefers-color-scheme: dark) {
  :host {
    --surface: rgba(40, 40, 42, 0.98);
    --text: #f5f5f7;
    --muted: #a1a1a6;
    --line: rgba(255, 255, 255, 0.14);
  }
}
[hidden] { display: none !important; }
.hover, .box {
  position: fixed;
  box-sizing: border-box;
  border: 2px solid var(--accent);
  background: var(--accent-wash);
  border-radius: 3px;
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
  padding: 2px 6px;
  border-radius: 4px;
  background: var(--accent);
  color: white;
  font-size: 11px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
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
  box-shadow: var(--shadow);
  cursor: pointer;
  pointer-events: auto;
}
.editor {
  position: fixed;
  width: 320px;
  box-sizing: border-box;
  padding: 10px;
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--surface);
  color: var(--text);
  box-shadow: var(--shadow);
  pointer-events: auto;
}
.editor-target {
  margin-bottom: 6px;
  color: var(--muted);
  font-size: 12px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
textarea {
  display: block;
  width: 100%;
  box-sizing: border-box;
  resize: vertical;
  padding: 6px 8px;
  border: 1px solid var(--line);
  border-radius: 6px;
  background: transparent;
  color: inherit;
  font: inherit;
  outline: none;
}
textarea:focus { border-color: var(--accent); }
.editor-row, .toolbar { display: flex; align-items: center; gap: 6px; }
.editor-row { margin-top: 8px; }
.grow { flex: 1; }
button {
  font: inherit;
  color: inherit;
  padding: 3px 10px;
  border: 1px solid var(--line);
  border-radius: 6px;
  background: transparent;
  cursor: pointer;
}
button:hover { background: var(--accent-wash); }
.save, .done { background: var(--accent); border-color: var(--accent); color: white; }
.save:hover, .done:hover { background: var(--accent); filter: brightness(1.08); }
.delete { color: #ff453a; }
.toolbar {
  position: fixed;
  left: 50%;
  bottom: 16px;
  transform: translateX(-50%);
  padding: 6px 6px 6px 12px;
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--surface);
  color: var(--text);
  box-shadow: var(--shadow);
  pointer-events: auto;
  white-space: nowrap;
}
.hint, .count { color: var(--muted); }
.count:empty { display: none; }
.tool-box[aria-pressed="true"] { background: var(--accent-wash); border-color: var(--accent); color: var(--accent); }
`;
