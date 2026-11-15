// The service worker: each tab's unsent review, review mode in tabs, the
// side panel's state, and the way to amux.
//
// The extension asks for no access to sites. Clicking its button, pressing
// its shortcut or using its menu grants it the current tab for that moment
// (activeTab), and only then does it put the overlay into the page.

import { Amux } from "./amux.ts";
import { captureRegion } from "./capture.ts";
import {
  dropImage,
  dropReview,
  emptyReview,
  getImage,
  getReview,
  putImage,
  putReview,
  reviewImages,
  sweepImages,
} from "./store.ts";
import type {
  AgentChoice,
  Destination,
  FromPanel,
  FromTab,
  PanelState,
  PendingReview,
  Snippet,
  ToPanel,
  ToTab,
} from "../shared/messages.ts";

declare const __AMUX_HOST__: string;

const amux = new Amux(__AMUX_HOST__);
const panels = new Map<chrome.runtime.Port, number | null>();
const reviewing = new Set<number>();
const outcomes = new Map<number, PanelState["outcome"]>();
const sending = new Set<number>();

chrome.runtime.onInstalled.addListener(() => {
  void sweepImages();
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({ id: "selection", title: "Send selection to an amux agent", contexts: ["selection"] });
    chrome.contextMenus.create({ id: "link", title: "Send link to an amux agent", contexts: ["link"] });
    chrome.contextMenus.create({ id: "page", title: "Send page to an amux agent", contexts: ["page"] });
  });
});
chrome.runtime.onStartup.addListener(() => void sweepImages());

function blocked(url: string | undefined): PanelState["blocked"] | "unknown" {
  if (!url) return "unknown";
  if (/^(chrome|edge|brave|arc|about|devtools|view-source|chrome-extension):/.test(url)) return "browser-page";
  if (/^https:\/\/(chrome\.google\.com\/webstore|chromewebstore\.google\.com)/.test(url)) return "browser-page";
  return null;
}

async function fileAccessMissing(url: string | undefined): Promise<boolean> {
  return !!url?.startsWith("file:") && !(await chrome.extension.isAllowedFileSchemeAccess());
}

function tell(tabId: number, message: ToTab) {
  chrome.tabs.sendMessage(tabId, message).catch(() => {
    // No overlay in the tab: it was never started there, or the page went.
  });
}

/** Puts the overlay in the tab, once: its scripts keep to one copy themselves. */
async function inject(tabId: number) {
  await chrome.scripting.executeScript({ target: { tabId }, files: ["page.js"], world: "MAIN" });
  await chrome.scripting.executeScript({ target: { tabId }, files: ["content.js"] });
}

async function toggleReview(tab: chrome.tabs.Tab) {
  const tabId = tab.id;
  if (tabId === undefined || blocked(tab.url) || (await fileAccessMissing(tab.url))) {
    schedule();
    return;
  }
  try {
    await inject(tabId);
    tell(tabId, { type: "toggle" });
  } catch (error) {
    console.warn("amux: could not review this page", error);
  }
}

async function startReview(tabId: number) {
  const tab = await chrome.tabs.get(tabId);
  if (blocked(tab.url) || (await fileAccessMissing(tab.url))) return;
  await inject(tabId);
  tell(tabId, { type: "start" });
}

chrome.action.onClicked.addListener((tab) => {
  // Opening the panel must happen straight from the click.
  if (tab.id !== undefined) void chrome.sidePanel.open({ tabId: tab.id });
  void toggleReview(tab);
});

chrome.commands.onCommand.addListener((command, tab) => {
  if (command !== "toggle-review" || !tab) return;
  if (tab.id !== undefined) void chrome.sidePanel.open({ tabId: tab.id });
  void toggleReview(tab);
});

chrome.contextMenus.onClicked.addListener((info, tab) => {
  if (!tab?.id) return;
  void chrome.sidePanel.open({ tabId: tab.id });
  const tabId = tab.id;
  const title = tab.title ?? "";
  const page = info.pageUrl ?? tab.url ?? "";
  let snippet: Snippet;
  if (info.menuItemId === "selection") {
    snippet = { id: crypto.randomUUID(), name: `From ${title || page}`, text: `${info.selectionText ?? ""}\n\n${page}` };
  } else if (info.menuItemId === "link") {
    snippet = { id: crypto.randomUUID(), name: "Link", text: info.linkUrl ?? "" };
  } else {
    snippet = { id: crypto.randomUUID(), name: title || "Page", text: page };
  }
  void (async () => {
    const review = (await getReview(tabId)) ?? emptyReview(tab.url, title);
    review.snippets.push(snippet);
    await putReview(tabId, review);
    outcomes.delete(tabId);
    schedule();
  })();
});

chrome.tabs.onRemoved.addListener((tabId) => {
  reviewing.delete(tabId);
  outcomes.delete(tabId);
  void dropReview(tabId).then(schedule);
});

chrome.tabs.onUpdated.addListener((tabId, change) => {
  if (change.status === "loading") reviewing.delete(tabId);
  if (change.status || change.url || change.title) schedule();
});

chrome.tabs.onActivated.addListener(() => schedule());

async function reviewFor(tab: chrome.tabs.Tab): Promise<PendingReview> {
  return (tab.id !== undefined ? await getReview(tab.id) : null) ?? emptyReview(tab.url, tab.title);
}

chrome.runtime.onMessage.addListener((message: FromTab, sender, respond) => {
  const tab = sender.tab;
  if (!tab?.id) return false;
  const tabId = tab.id;
  void (async () => {
    const review = await reviewFor(tab);
    switch (message.type) {
      case "hello":
        respond({ comments: review.url === tab.url ? review.comments : [] });
        return;
      case "added":
        if (review.comments.length === 0) {
          review.url = tab.url ?? review.url;
          review.title = tab.title ?? review.title;
        }
        review.comments.push(message.comment);
        break;
      case "edited":
        for (const comment of review.comments) if (comment.id === message.id) comment.text = message.text;
        break;
      case "deleted":
        review.comments = review.comments.filter((comment) => comment.id !== message.id);
        await dropImage(message.id);
        break;
      case "capture":
        try {
          const shot = await captureRegion(tab.windowId, message.region);
          await putImage(message.id, shot.png, shot.thumbnail);
          respond(true);
        } catch (error) {
          console.warn("amux: could not capture the box", error);
          respond(false);
        }
        return;
      case "stopped":
        reviewing.delete(tabId);
        schedule();
        respond(null);
        return;
    }
    await putReview(tabId, review);
    outcomes.delete(tabId);
    reviewing.add(tabId);
    tell(tabId, { type: "comments", comments: review.comments });
    schedule();
    respond(null);
  })();
  return true;
});

chrome.runtime.onConnect.addListener((port) => {
  if (port.name !== "panel") return;
  panels.set(port, null);
  void amux.watch();
  port.onMessage.addListener((message: FromPanel) => void fromPanel(port, message));
  port.onDisconnect.addListener(() => {
    panels.delete(port);
    if (panels.size === 0) amux.unwatch();
  });
});

async function fromPanel(port: chrome.runtime.Port, message: FromPanel) {
  switch (message.type) {
    case "watch":
      panels.set(port, message.tabId);
      await push(port, message.tabId);
      return;
    case "review":
      await startReview(message.tabId).catch((error) => console.warn("amux:", error));
      reviewing.add(message.tabId);
      break;
    case "delete": {
      const review = await getReview(message.tabId);
      if (!review) return;
      review.comments = review.comments.filter((comment) => comment.id !== message.id);
      await dropImage(message.id);
      await putReview(message.tabId, review);
      tell(message.tabId, { type: "comments", comments: review.comments });
      break;
    }
    case "deleteSnippet": {
      const review = await getReview(message.tabId);
      if (!review) return;
      review.snippets = review.snippets.filter((snippet) => snippet.id !== message.id);
      await putReview(message.tabId, review);
      break;
    }
    case "focus":
      await startReview(message.tabId).catch(() => {});
      tell(message.tabId, { type: "focus", id: message.id });
      break;
    case "send":
      await send(message.tabId, message.destination, message.message);
      break;
    case "fileAccess":
      await chrome.tabs.create({ url: `chrome://extensions/?id=${chrome.runtime.id}` });
      return;
  }
  schedule();
}

async function send(tabId: number, destination: Destination, message: string) {
  const review = await getReview(tabId);
  if (!review || (review.comments.length === 0 && review.snippets.length === 0)) return;
  sending.add(tabId);
  schedule();
  try {
    const images = await reviewImages(review);
    const name = await amux.send(destination, review, message, images);
    await dropReview(tabId);
    outcomes.set(tabId, { sent: name });
    tell(tabId, { type: "comments", comments: [] });
    tell(tabId, { type: "stop" });
    await remember(review.url, destination);
  } catch (error) {
    outcomes.set(tabId, { failed: error instanceof Error ? error.message : String(error) });
  } finally {
    sending.delete(tabId);
    schedule();
  }
}

function origin(url: string): string {
  try {
    const parsed = new URL(url);
    return parsed.protocol === "file:" ? "file:" : parsed.origin;
  } catch {
    return "";
  }
}

async function remember(url: string, destination: Destination) {
  const at = origin(url);
  if (at) await chrome.storage.local.set({ [`destination:${at}`]: destination });
}

/**
 * Where a review on this page most likely goes: for a local file, the agent
 * whose folder holds it; else the one last sent to from this site.
 */
async function suggest(url: string, agents: AgentChoice[]): Promise<Destination | null> {
  if (url.startsWith("file:")) {
    const path = decodeURIComponent(new URL(url).pathname);
    const holders = agents
      .filter((agent) => agent.local && agent.cwd && path.startsWith(agent.cwd.endsWith("/") ? agent.cwd : `${agent.cwd}/`))
      .sort((a, b) => Number(a.closed) - Number(b.closed) || b.cwd.length - a.cwd.length);
    const holder = holders[0];
    if (holder) return { kind: "agent", profile: holder.profile, host: holder.host, agent: holder.agent };
  }
  const at = origin(url);
  if (!at) return null;
  const held = await chrome.storage.local.get(`destination:${at}`);
  const last = held[`destination:${at}`] as Destination | undefined;
  if (last?.kind === "agent" && !agents.some((agent) => agent.agent === last.agent)) return null;
  return last ?? null;
}

async function state(tabId: number): Promise<PanelState> {
  const tab = await chrome.tabs.get(tabId).catch(() => null);
  const review = tab ? await reviewFor(tab) : emptyReview();
  const thumbnails: Record<string, string> = {};
  for (const comment of review.comments) {
    if (!comment.region) continue;
    const image = await getImage(comment.id);
    if (image) thumbnails[comment.id] = image.thumbnail;
  }
  const agents = await amux.agents().catch(() => []);
  const kind = blocked(tab?.url);
  return {
    tabId,
    blocked: kind === "unknown" ? null : kind ?? ((await fileAccessMissing(tab?.url)) ? "file-access" : null),
    reviewing: reviewing.has(tabId),
    review,
    thumbnails,
    reach: amux.reach(),
    profiles: amux.profileList(),
    agents,
    folders: amux.folderList(),
    suggested: await suggest(review.url || tab?.url || "", agents),
    outcome: outcomes.get(tabId) ?? null,
    sending: sending.has(tabId),
  };
}

async function push(port: chrome.runtime.Port, tabId: number) {
  const message: ToPanel = { type: "state", state: await state(tabId) };
  try {
    port.postMessage(message);
  } catch {
    panels.delete(port);
  }
}

let pending: ReturnType<typeof setTimeout> | null = null;

/** Sends every open panel its state, once for a burst of changes. */
function schedule() {
  if (pending) return;
  pending = setTimeout(() => {
    pending = null;
    for (const [port, tabId] of panels) if (tabId !== null) void push(port, tabId);
  }, 50);
}

amux.onChange(schedule);
