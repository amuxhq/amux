// Runs in the page's own world, where the page's scripts keep what they know
// of each element: React's development builds say which file and line drew
// a component, Vue names its component's file, Svelte its file and line.
// The content script asks by marking an element and sending an event; this
// answers with "path:line", or nothing. It reads, never writes, the page.

declare global {
  interface Window {
    __amuxSource?: true;
  }
}

interface ReactFiber {
  _debugSource?: { fileName?: string; lineNumber?: number };
  return?: ReactFiber | null;
}

/** Shortens an absolute path to the part after a source folder, when it has one. */
function relative(path: string): string {
  const cleaned = path.replace(/^webpack:\/\/\/?|^file:\/\//, "").replace(/\?.*$/, "");
  const at = cleaned.search(/\/(src|app|pages|components|lib)\//);
  return at >= 0 ? cleaned.slice(at + 1) : cleaned;
}

function fromReact(element: Element): string {
  const key = Object.keys(element).find((name) => name.startsWith("__reactFiber$"));
  if (!key) return "";
  let fiber = (element as unknown as Record<string, ReactFiber>)[key] ?? null;
  for (let depth = 0; fiber && depth < 30; depth += 1) {
    const source = fiber._debugSource;
    if (source?.fileName) {
      return source.lineNumber ? `${relative(source.fileName)}:${source.lineNumber}` : relative(source.fileName);
    }
    fiber = fiber.return ?? null;
  }
  return "";
}

function fromVue(element: Element): string {
  let current: Element | null = element;
  while (current) {
    const component = (current as unknown as { __vueParentComponent?: { type?: { __file?: string } } })
      .__vueParentComponent;
    if (component?.type?.__file) return relative(component.type.__file);
    current = current.parentElement;
  }
  return "";
}

function fromSvelte(element: Element): string {
  let current: Element | null = element;
  while (current) {
    const meta = (current as unknown as { __svelte_meta?: { loc?: { file?: string; line?: number } } }).__svelte_meta;
    if (meta?.loc?.file) {
      return meta.loc.line ? `${relative(meta.loc.file)}:${meta.loc.line + 1}` : relative(meta.loc.file);
    }
    current = current.parentElement;
  }
  return "";
}

if (!window.__amuxSource) {
  window.__amuxSource = true;
  document.addEventListener("amux:source", (event) => {
    const id = (event as CustomEvent<string>).detail;
    const element = document.querySelector(`[data-amux-probe="${CSS.escape(id)}"]`);
    let source = "";
    if (element) {
      try {
        source = fromReact(element) || fromVue(element) || fromSvelte(element);
      } catch {
        source = "";
      }
    }
    // A string crosses between the page's world and the extension's intact.
    document.dispatchEvent(new CustomEvent("amux:source-answer", { detail: JSON.stringify({ id, source }) }));
  });
}

export {};
