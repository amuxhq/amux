// The way to amux: native messaging to `amux internal browser`, which makes
// each call as raw gRPC on the installation's socket or a profile's, and
// streams the answers back. Calls are numbered; a response message too large
// for one frame arrives in parts, joined here. Every call names who answers
// it by its authority, which the bridge passes to the node behind the socket:
// a host's id to carry it there, or `NODE` for the node itself.

/** Where a call goes: the installation's front door, or a profile by its id in hex. */
export type Target = "installation" | string;

/** How a call ended: gRPC's code, its message, and its details in base64. */
export interface End {
  code: number;
  message: string;
  details: string;
}

/** Why amux could not be reached at all. */
export type Unreachable =
  /** No host manifest: amux is not installed, or is older than this extension. */
  | "not-installed"
  /** The host ran and stopped: amux is installed but its bridge failed. */
  | "stopped";

export class CallError extends Error {
  constructor(readonly end: End) {
    super(end.message || `amux ended the call with code ${end.code}`);
  }
}

/** gRPC's "unavailable": the daemon is not answering. */
export const UNAVAILABLE = 14;

/**
 * The revision of the framing this extension speaks: every call carries an
 * authority. An amux whose bridge says less is older than this extension.
 */
export const FRAMING = 2;

/** The authority of a call the node behind the socket answers itself. */
export const NODE = "amux.local";

interface Open {
  onMessage(message: string): void;
  onEnd(end: End): void;
  part: string[];
}

/** Joins a message's parts: base64 of consecutive byte ranges. */
export function joinParts(parts: readonly string[]): string {
  if (parts.length === 1) return parts[0]!;
  const bytes = parts.map((part) => Uint8Array.from(atob(part), (c) => c.charCodeAt(0)));
  const total = bytes.reduce((sum, chunk) => sum + chunk.length, 0);
  const joined = new Uint8Array(total);
  let at = 0;
  for (const chunk of bytes) {
    joined.set(chunk, at);
    at += chunk.length;
  }
  return toBase64(joined);
}

export function toBase64(bytes: Uint8Array): string {
  let binary = "";
  const step = 0x8000;
  for (let at = 0; at < bytes.length; at += step) {
    binary += String.fromCharCode(...bytes.subarray(at, at + step));
  }
  return btoa(binary);
}

/** A frame as the bridge sends it. */
type Frame =
  /**
   * `framing`: the revision of the framing the bridge speaks. A revision may
   * drop what an earlier one allowed, as 2 made every call name its authority,
   * so a bridge below `FRAMING` cannot serve this extension.
   */
  | { hello: { version: string; framing: number } }
  | { id: number; message: string; more?: boolean }
  | { id: number; end: End };

/** The native port, opened on first use and again after it closes. */
export class Bridge {
  private port: chrome.runtime.Port | null = null;
  private next = 1;
  private open = new Map<number, Open>();
  /** The amux version the bridge said it is, once it said. */
  version: string | null = null;
  /** The revision of the framing the bridge said it speaks, once it said. */
  framing: number | null = null;
  /** Why the last port closed, if it did. */
  unreachable: Unreachable | null = null;
  private listeners = new Set<() => void>();
  /** Whether to keep the port open with no call open, while a panel watches. */
  private held = false;

  constructor(private readonly host: string) {}

  /**
   * Keeps the port open between calls, or lets it close once no call is
   * open: an open native port keeps the service worker and the bridge
   * process running.
   */
  hold(held: boolean) {
    this.held = held;
    this.release();
  }

  private release() {
    if (this.held || this.open.size > 0 || !this.port) return;
    const port = this.port;
    this.forget();
    port.disconnect();
  }

  /** What a closed port said of itself, gone with it. */
  private forget() {
    this.port = null;
    this.version = null;
    this.framing = null;
  }

  /** Hears when reachability or the version changes. */
  onChange(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  private changed() {
    for (const listener of this.listeners) listener();
  }

  private connect(): chrome.runtime.Port {
    if (this.port) return this.port;
    const port = chrome.runtime.connectNative(this.host);
    this.port = port;
    port.onMessage.addListener((frame: Frame) => this.receive(frame));
    port.onDisconnect.addListener(() => {
      const error = chrome.runtime.lastError?.message ?? "";
      this.forget();
      this.unreachable = /not found|forbidden/i.test(error) ? "not-installed" : "stopped";
      const ended: End = { code: UNAVAILABLE, message: error || "amux's bridge stopped", details: "" };
      const open = [...this.open.values()];
      this.open.clear();
      for (const call of open) call.onEnd(ended);
      this.changed();
    });
    return port;
  }

  private receive(frame: Frame) {
    if ("hello" in frame) {
      this.version = frame.hello.version;
      this.framing = frame.hello.framing;
      this.unreachable = null;
      this.changed();
      return;
    }
    const call = this.open.get(frame.id);
    if (!call) return;
    if ("end" in frame) {
      this.open.delete(frame.id);
      call.onEnd(frame.end);
      this.release();
      return;
    }
    call.part.push(frame.message);
    if (!frame.more) {
      const message = joinParts(call.part);
      call.part = [];
      call.onMessage(message);
    }
  }

  /**
   * Opens a stream: `onMessage` hears each response message (base64),
   * `onEnd` how it ended. `authority` names who answers it: a host's id in
   * the form every client writes it, or `NODE`. Answers a function that
   * cancels it.
   */
  stream(
    target: Target,
    authority: string,
    method: string,
    body: string,
    onMessage: (message: string) => void,
    onEnd: (end: End) => void,
  ): () => void {
    const id = this.next++;
    this.open.set(id, { onMessage, onEnd, part: [] });
    const failed = (error: unknown) => {
      if (this.open.delete(id)) onEnd({ code: UNAVAILABLE, message: String(error), details: "" });
    };
    try {
      this.connect().postMessage({ id, call: { target, method, body, authority } });
    } catch (error) {
      failed(error);
    }
    return () => {
      if (this.open.delete(id)) {
        this.port?.postMessage({ id, cancel: true });
        this.release();
      }
    };
  }

  /** A call answered by one message, by whoever `authority` names. */
  unary(target: Target, authority: string, method: string, body: string): Promise<string> {
    return new Promise((resolve, reject) => {
      let answer: string | null = null;
      this.stream(
        target,
        authority,
        method,
        body,
        (message) => {
          answer = message;
        },
        (end) => {
          if (end.code !== 0) reject(new CallError(end));
          else if (answer === null) reject(new CallError({ code: 2, message: "amux answered nothing", details: "" }));
          else resolve(answer);
        },
      );
    });
  }
}
