// The amux client crates, compiled to WebAssembly (crates/browser-core):
// everything that knows `amux.v1`. One call, JSON in and JSON out.

interface Exports {
  memory: WebAssembly.Memory;
  alloc(len: number): number;
  free(at: number, len: number): void;
  call(at: number, len: number): bigint;
}

let loaded: Promise<Exports> | null = null;

function load(): Promise<Exports> {
  loaded ??= (async () => {
    const bytes = await (await fetch(chrome.runtime.getURL("browser_core.wasm"))).arrayBuffer();
    const { instance } = await WebAssembly.instantiate(bytes, {});
    return instance.exports as unknown as Exports;
  })();
  return loaded;
}

export class CoreError extends Error {}

/** Asks browser-core; answers its `ok` value or throws its error. */
export async function core<T = unknown>(request: { op: string } & Record<string, unknown>): Promise<T> {
  const wasm = await load();
  const input = new TextEncoder().encode(JSON.stringify(request));
  const at = wasm.alloc(input.length);
  new Uint8Array(wasm.memory.buffer, at, input.length).set(input);
  const packed = wasm.call(at, input.length);
  const place = Number(packed >> 32n);
  const length = Number(packed & 0xffffffffn);
  const text = new TextDecoder().decode(new Uint8Array(wasm.memory.buffer, place, length));
  wasm.free(place, length);
  const answer = JSON.parse(text) as { ok?: T; error?: string };
  if (answer.error !== undefined) throw new CoreError(answer.error);
  return answer.ok as T;
}
