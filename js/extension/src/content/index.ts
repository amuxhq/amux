// The overlay's host in a tab: it hands comments and screenshot requests to
// the service worker, and asks the page-world script where the page's own
// code says a block was drawn from. Injected on demand, so it keeps to one
// copy per page however many times it is injected.

import { createOverlay, type Comment, type Host, type Overlay } from "@amux/page-review";
import type { FromTab, ToTab } from "../shared/messages.ts";

declare global {
  interface Window {
    __amuxReview?: Overlay;
  }
}

/** How long the page-world script has to say where a block came from. */
const SOURCE_PATIENCE_MS = 150;

function send<T = unknown>(message: FromTab): Promise<T> {
  return chrome.runtime.sendMessage(message) as Promise<T>;
}

function sourceOf(element: Element): Promise<string> {
  return new Promise((resolve) => {
    const id = crypto.randomUUID();
    element.setAttribute("data-amux-probe", id);
    const answered = (event: Event) => {
      // A string crosses between the page's world and this one intact.
      let detail: { id?: string; source?: string } = {};
      try {
        detail = JSON.parse(String((event as CustomEvent<string>).detail));
      } catch {
        return;
      }
      if (detail.id !== id) return;
      finish(detail.source ?? "");
    };
    const finish = (source: string) => {
      clearTimeout(timer);
      document.removeEventListener("amux:source-answer", answered);
      element.removeAttribute("data-amux-probe");
      resolve(source);
    };
    const timer = setTimeout(() => finish(""), SOURCE_PATIENCE_MS);
    document.addEventListener("amux:source-answer", answered);
    document.dispatchEvent(new CustomEvent("amux:source", { detail: id }));
  });
}

function mount(): Overlay {
  const host: Host = {
    added: (comment: Comment) => void send({ type: "added", comment }),
    edited: (id, text) => void send({ type: "edited", id, text }),
    deleted: (id) => void send({ type: "deleted", id }),
    capture: (id, region) => send<boolean>({ type: "capture", id, region }).catch(() => false),
    stopped: () => void send({ type: "stopped" }),
    sourceOf,
  };
  const overlay = createOverlay(host);
  chrome.runtime.onMessage.addListener((message: ToTab) => {
    switch (message.type) {
      case "start":
        overlay.start();
        break;
      case "stop":
        overlay.stop();
        break;
      case "toggle":
        if (overlay.active) overlay.stop();
        else overlay.start();
        break;
      case "comments":
        overlay.setComments(message.comments);
        break;
      case "focus":
        overlay.focus(message.id);
        break;
    }
  });
  void send<{ comments: Comment[] }>({ type: "hello" }).then((answer) => {
    if (answer?.comments) overlay.setComments(answer.comments);
  });
  return overlay;
}

window.__amuxReview ??= mount();
