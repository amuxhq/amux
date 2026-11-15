// amux as the extension sees it: the profiles, each profile's fleet kept
// from its inventory stream, and sending a page review to an agent. This is
// the part the drivers play in the other clients; browser-core does every
// encoding and decoding.

import { Bridge, CallError, UNAVAILABLE } from "./bridge.ts";
import { core } from "./core.ts";
import type { AgentChoice, Destination, Folder, PendingReview, Reach } from "../shared/messages.ts";

interface Profile {
  id: string;
  label: string;
  localHost: string;
  fleet: number;
  cancel: () => void;
}

/** How long to wait before subscribing again after a stream ends. */
const RETRY_MS = [500, 1000, 2000, 5000, 10000];

/** How long a new agent may take to appear in the fleet before its review is sent. */
const NEW_AGENT_PATIENCE_MS = 15000;

export interface Image {
  png: Uint8Array;
}

function randomHex(bytes: number): string {
  return Array.from(crypto.getRandomValues(new Uint8Array(bytes)), (byte) =>
    byte.toString(16).padStart(2, "0"),
  ).join("");
}

export class Amux {
  readonly bridge: Bridge;
  private profiles = new Map<string, Profile>();
  private folders: Folder[] = [];
  private watching = false;
  private retries = 0;
  private retry: ReturnType<typeof setTimeout> | null = null;
  private daemon: "unknown" | "answering" | "not-running" = "unknown";
  private stopDetail = "";
  private listeners = new Set<() => void>();

  constructor(host: string) {
    this.bridge = new Bridge(host);
    this.bridge.onChange(() => this.changed());
  }

  onChange(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  private changed() {
    for (const listener of this.listeners) listener();
  }

  reach(): Reach {
    if (this.bridge.unreachable === "not-installed") return { status: "not-installed" };
    if (this.daemon === "not-running") return { status: "not-running" };
    if (this.bridge.unreachable === "stopped" && this.daemon !== "answering") {
      return { status: "stopped", detail: this.stopDetail };
    }
    if (this.bridge.version && this.daemon === "answering") return { status: "ok", version: this.bridge.version };
    return { status: "connecting" };
  }

  profileList(): Array<{ id: string; label: string }> {
    return [...this.profiles.values()].map(({ id, label }) => ({ id, label }));
  }

  /** Keeps every profile's fleet while something is watching. */
  async watch(): Promise<void> {
    if (this.watching) return;
    this.watching = true;
    await this.open();
  }

  /** Stops every stream, so the service worker may sleep. */
  unwatch() {
    this.watching = false;
    if (this.retry) clearTimeout(this.retry);
    this.retry = null;
    for (const profile of this.profiles.values()) {
      profile.cancel();
      void core({ op: "fleetDrop", fleet: profile.fleet });
    }
    this.profiles.clear();
  }

  private again() {
    if (!this.watching || this.retry) return;
    const wait = RETRY_MS[Math.min(this.retries, RETRY_MS.length - 1)]!;
    this.retries += 1;
    this.retry = setTimeout(() => {
      this.retry = null;
      void this.open();
    }, wait);
  }

  private async open() {
    if (!this.watching) return;
    let listed: Array<{ id: string; label: string }>;
    try {
      const message = await this.bridge.unary("installation", "/amux.v1.InstallationService/ListProfiles", "");
      listed = await core({ op: "profiles", message });
      this.daemon = "answering";
    } catch (error) {
      if (error instanceof CallError && error.end.code === UNAVAILABLE && this.bridge.version) {
        this.daemon = "not-running";
      } else {
        this.stopDetail = error instanceof Error ? error.message : String(error);
      }
      this.changed();
      this.again();
      return;
    }
    this.retries = 0;
    const wanted = new Set(listed.map((profile) => profile.id));
    for (const [id, profile] of this.profiles) {
      if (!wanted.has(id)) {
        profile.cancel();
        void core({ op: "fleetDrop", fleet: profile.fleet });
        this.profiles.delete(id);
      }
    }
    for (const { id, label } of listed) {
      if (!this.profiles.has(id)) await this.subscribe(id, label);
    }
    await this.loadFolders();
    this.changed();
  }

  private async subscribe(id: string, label: string) {
    let localHost = "";
    try {
      const message = await this.bridge.unary(id, "/amux.v1.ProfileAdminService/GetDeviceIdentity", "");
      localHost = (await core<{ hostId: string }>({ op: "deviceIdentity", message })).hostId;
    } catch {
      // Without it every agent reads as another host's; the picker still works.
    }
    const fleet = await core<number>({ op: "fleetNew" });
    const profile: Profile = { id, label: label || "amux", localHost, fleet, cancel: () => {} };
    this.profiles.set(id, profile);
    profile.cancel = this.bridge.stream(
      id,
      "/amux.v1.AgentService/SubscribeInventory",
      "",
      (message) => {
        void core({ op: "fleetApply", fleet, message }).then(() => this.changed());
      },
      () => {
        // The stream ended: start the fleet again from a fresh listing.
        if (this.profiles.get(id) === profile) {
          this.profiles.delete(id);
          void core({ op: "fleetDrop", fleet });
        }
        this.changed();
        this.again();
      },
    );
  }

  private async loadFolders() {
    const folders: Folder[] = [];
    for (const profile of this.profiles.values()) {
      try {
        const request = await core<string>({ op: "listRepositories" });
        const message = await this.bridge.unary(profile.id, "/amux.v1.AgentService/ListRepositories", request);
        const listed = await core<{ recent: Folder[]; repositories: Folder[] }>({ op: "repositories", message });
        const seen = new Set<string>();
        for (const folder of [...listed.repositories, ...listed.recent]) {
          if (seen.has(folder.path)) continue;
          seen.add(folder.path);
          folders.push({ profile: profile.id, path: folder.path, name: folder.name });
        }
      } catch {
        // A host that cannot list folders offers none.
      }
    }
    this.folders = folders;
  }

  folderList(): Folder[] {
    return this.folders;
  }

  async agents(): Promise<AgentChoice[]> {
    const all: AgentChoice[] = [];
    for (const profile of this.profiles.values()) {
      const choices = await core<AgentChoice[]>({
        op: "picker",
        fleet: profile.fleet,
        localHost: profile.localHost,
        profile: profile.id,
      });
      all.push(...choices);
    }
    return all;
  }

  /**
   * Sends a tab's review to its destination: creates the agent first when
   * it is a new one, stores each screenshot with the agent, then sends the
   * words with the review and snippets after them. Answers the name it was
   * sent to.
   */
  async send(
    destination: Destination,
    review: PendingReview,
    message: string,
    images: Map<string, Image>,
  ): Promise<string> {
    const profile = this.profiles.get(destination.profile);
    if (!profile) throw new Error("That profile is not open any more.");
    let host: string;
    let agent: string;
    if (destination.kind === "new") {
      host = profile.localHost;
      agent = randomHex(16);
      const request = await core<string>({
        op: "createAgent",
        fleet: profile.fleet,
        localHost: host,
        agent,
        cwd: destination.cwd,
      });
      await this.bridge.unary(profile.id, "/amux.v1.AgentService/CreateAgent", request);
      await this.until(profile, host, agent);
    } else {
      host = destination.host;
      agent = destination.agent;
    }

    const comments = [];
    for (const comment of review.comments) {
      let image = null;
      const shot = images.get(comment.id);
      if (shot) {
        const request = await core<string>({
          op: "putBlob",
          agent,
          name: `region-${comments.length + 1}.png`,
          mime: "image/png",
          bytes: toBase64(shot.png),
        });
        const stored = await this.bridge.unary(profile.id, "/amux.v1.AgentService/PutBlob", request);
        image = await core({ op: "blob", message: stored });
      }
      comments.push({
        text: comment.text,
        element: comment.element,
        quote: comment.quote,
        image,
      });
    }
    const page = review.comments.length > 0 ? { url: review.url, title: review.title, comments } : null;
    const placeholders = "￼".repeat((page ? 1 : 0) + review.snippets.length);
    const words = message.trim();
    const text = words ? `${words}\n\n${placeholders}` : placeholders;
    const request = await core<string>({
      op: "sendPrompt",
      fleet: profile.fleet,
      host,
      agent,
      inputId: randomHex(16),
      text,
      page,
      texts: review.snippets.map(({ name, text }) => ({ name, text })),
    });
    const answer = await this.bridge.unary(profile.id, "/amux.v1.SessionService/SendInput", request);
    const verdict = await core<{ accepted: boolean; refusal?: string; detail?: string }>({
      op: "verdict",
      message: answer,
    });
    if (!verdict.accepted) {
      throw new Error(verdict.detail || `The agent did not take it (${verdict.refusal ?? "refused"}).`);
    }
    const choices = await this.agents();
    return choices.find((choice) => choice.agent === agent)?.name ?? "the agent";
  }

  /** Waits for a new agent to appear in its profile's fleet. */
  private async until(profile: Profile, host: string, agent: string) {
    const deadline = Date.now() + NEW_AGENT_PATIENCE_MS;
    while (Date.now() < deadline) {
      const kind = await core<string | null>({ op: "agentKind", fleet: profile.fleet, host, agent });
      if (kind) return;
      await new Promise((done) => setTimeout(done, 100));
    }
    throw new Error("The new agent did not start in time.");
  }
}

function toBase64(bytes: Uint8Array): string {
  let binary = "";
  for (let at = 0; at < bytes.length; at += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(at, at + 0x8000));
  }
  return btoa(binary);
}
