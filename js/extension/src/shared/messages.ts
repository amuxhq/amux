// What the extension's parts say to each other: the content script in a tab,
// the side panel, and the service worker that holds the state and the way to
// amux. All three ship together, so none of this crosses versions.

import type { Comment, Region } from "@amux/page-review";

export type { Comment, Region };

/** Text sent from a page: a selection, or a page's or link's address. */
export interface Snippet {
  id: string;
  name: string;
  text: string;
}

/** A tab's comments and snippets, kept until they are sent. */
export interface PendingReview {
  url: string;
  title: string;
  comments: Comment[];
  snippets: Snippet[];
}

/** An agent the picker offers. */
export interface AgentChoice {
  profile: string;
  host: string;
  agent: string;
  /** What a person reads it as: its title, else its name. */
  name: string;
  /** The name it is addressed by. */
  handle: string;
  hostName: string;
  cwd: string;
  repository: string;
  closed: boolean;
  needsYou: boolean;
  local: boolean;
}

/** A folder a new agent can start in. */
export interface Folder {
  profile: string;
  path: string;
  name: string;
}

export type Destination =
  | { kind: "agent"; profile: string; host: string; agent: string }
  | { kind: "new"; profile: string; cwd: string };

/** Whether amux answers. */
export type Reach =
  | { status: "connecting" }
  | { status: "ok"; version: string }
  /** No host manifest: amux is not installed here, or is older than this extension. */
  | { status: "not-installed" }
  /** The bridge runs but no daemon answers. */
  | { status: "not-running" }
  | { status: "stopped"; detail: string };

export interface PanelState {
  tabId: number | null;
  /** Why this tab cannot be reviewed, when it cannot. */
  blocked: "browser-page" | "file-access" | null;
  reviewing: boolean;
  review: PendingReview;
  thumbnails: Record<string, string>;
  reach: Reach;
  profiles: Array<{ id: string; label: string }>;
  agents: AgentChoice[];
  folders: Folder[];
  suggested: Destination | null;
  /** The last send's outcome, until the next change. */
  outcome: { sent: string } | { failed: string } | null;
  sending: boolean;
}

/** From a tab's content script to the service worker. */
export type FromTab =
  | { type: "hello" }
  | { type: "added"; comment: Comment }
  | { type: "edited"; id: string; text: string }
  | { type: "deleted"; id: string }
  | { type: "capture"; id: string; region: Region }
  | { type: "stopped" };

/** From the service worker to a tab's content script. */
export type ToTab =
  | { type: "start" }
  | { type: "stop" }
  | { type: "toggle" }
  | { type: "comments"; comments: Comment[] }
  | { type: "focus"; id: string };

/** From the side panel to the service worker. */
export type FromPanel =
  | { type: "watch"; tabId: number }
  | { type: "review"; tabId: number }
  | { type: "delete"; tabId: number; id: string }
  | { type: "deleteSnippet"; tabId: number; id: string }
  | { type: "focus"; tabId: number; id: string }
  | { type: "send"; tabId: number; destination: Destination; message: string }
  | { type: "fileAccess" };

/** From the service worker to the side panel. */
export type ToPanel = { type: "state"; state: PanelState };
