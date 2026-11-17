// The look every amux surface on the web shares: the overlay on a page and
// the extension's side panel. Colours, radii and type are custom properties
// under one selector (`:host` in the overlay's shadow root, `:root` in the
// panel), and the controls both draw (buttons, fields, keys) are styled once
// here, so the two never drift apart.

/** The palette and metrics, light and dark, as custom properties of `scope`. */
export function tokens(scope: string): string {
  return `
${scope} {
  color-scheme: light dark;
  --font: 13px/1.45 -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif;
  --mono: 12px ui-monospace, SFMono-Regular, Menlo, monospace;
  --accent: #0a84ff;
  --accent-text: #ffffff;
  --accent-wash: rgba(10, 132, 255, 0.10);
  --ring: 0 0 0 3px rgba(10, 132, 255, 0.28);
  --ground: #ffffff;
  --surface: #ffffff;
  --subtle: #f4f4f5;
  --hover: rgba(24, 24, 27, 0.05);
  --border: #e4e4e7;
  --border-strong: #d4d4d8;
  --text: #18181b;
  --muted: #71717a;
  --danger: #dc2626;
  --success: #15803d;
  --success-wash: #f0fdf4;
  --danger-wash: #fef2f2;
  --notice-wash: #fffbeb;
  --notice: #b45309;
  --radius: 8px;
  --radius-sm: 6px;
  --shadow-pop: 0 10px 30px rgba(0, 0, 0, 0.12), 0 2px 6px rgba(0, 0, 0, 0.08), 0 0 0 1px rgba(0, 0, 0, 0.06);
}
@media (prefers-color-scheme: dark) {
  ${scope} {
    --accent-wash: rgba(10, 132, 255, 0.18);
    --ground: #18181b;
    --surface: #1f1f23;
    --subtle: #27272a;
    --hover: rgba(255, 255, 255, 0.06);
    --border: #2e2e33;
    --border-strong: #3f3f46;
    --text: #fafafa;
    --muted: #a1a1aa;
    --danger: #f87171;
    --success: #4ade80;
    --success-wash: rgba(74, 222, 128, 0.10);
    --danger-wash: rgba(248, 113, 113, 0.10);
    --notice-wash: rgba(251, 191, 36, 0.10);
    --notice: #fbbf24;
    --shadow-pop: 0 10px 30px rgba(0, 0, 0, 0.5), 0 2px 6px rgba(0, 0, 0, 0.3), 0 0 0 1px rgba(255, 255, 255, 0.08);
  }
}`;
}

/** The controls both surfaces draw. */
export const CONTROLS = `
.btn {
  display: inline-flex; align-items: center; justify-content: center; gap: 6px;
  height: 30px; padding: 0 12px; box-sizing: border-box;
  font: inherit; font-weight: 500; white-space: nowrap;
  color: var(--text); background: var(--surface);
  border: 1px solid var(--border-strong); border-radius: var(--radius-sm);
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.04);
  cursor: pointer; transition: background 0.1s, border-color 0.1s, opacity 0.1s;
}
.btn:hover { background: var(--subtle); }
.btn:focus-visible { outline: none; box-shadow: var(--ring); }
.btn:disabled { opacity: 0.45; cursor: default; }
.btn-primary { color: var(--accent-text); background: var(--accent); border-color: transparent; }
.btn-primary:hover { background: var(--accent); filter: brightness(1.08); }
.btn-primary:disabled:hover { filter: none; }
.btn-ghost { background: transparent; border-color: transparent; box-shadow: none; color: var(--muted); }
.btn-ghost:hover { background: var(--hover); color: var(--text); }
.btn-danger { background: transparent; border-color: transparent; box-shadow: none; color: var(--danger); }
.btn-danger:hover { background: var(--danger-wash); }
.btn-sm { height: 26px; padding: 0 9px; font-size: 12px; }
.btn-icon { width: 26px; height: 26px; padding: 0; }
.field {
  display: block; width: 100%; box-sizing: border-box;
  padding: 7px 10px; font: inherit; color: var(--text);
  background: var(--surface); border: 1px solid var(--border-strong); border-radius: var(--radius-sm);
  outline: none; transition: border-color 0.1s, box-shadow 0.1s;
}
.field::placeholder { color: var(--muted); }
.field:focus, .field:focus-within, .field[aria-expanded="true"] { border-color: var(--accent); box-shadow: var(--ring); }
textarea.field { resize: none; min-height: 60px; }
.kbd {
  display: inline-block; padding: 0 5px; min-width: 18px; box-sizing: border-box; text-align: center;
  font: 500 11px/18px -apple-system, BlinkMacSystemFont, system-ui, sans-serif; color: var(--muted);
  background: var(--subtle); border: 1px solid var(--border); border-bottom-width: 2px; border-radius: 5px;
  white-space: nowrap;
}
`;
