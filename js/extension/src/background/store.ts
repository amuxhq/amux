// Each tab's unsent review. The comments and snippets live in session
// storage, which the browser keeps while it runs and drops when it closes;
// the service worker is stopped whenever it is idle, so nothing is kept in
// its memory. Screenshots, which session storage has too little room for,
// live in IndexedDB beside them and go when their tab's review does.

import type { PendingReview } from "../shared/messages.ts";
import type { Image } from "./amux.ts";

const key = (tabId: number) => `review:${tabId}`;

export function emptyReview(url = "", title = ""): PendingReview {
  return { url, title, comments: [], snippets: [] };
}

export async function getReview(tabId: number): Promise<PendingReview | null> {
  const held = await chrome.storage.session.get(key(tabId));
  return (held[key(tabId)] as PendingReview | undefined) ?? null;
}

export async function putReview(tabId: number, review: PendingReview): Promise<void> {
  await chrome.storage.session.set({ [key(tabId)]: review });
}

export async function dropReview(tabId: number): Promise<void> {
  const review = await getReview(tabId);
  await chrome.storage.session.remove(key(tabId));
  for (const comment of review?.comments ?? []) await dropImage(comment.id);
}

interface Stored {
  id: string;
  png: Uint8Array;
  thumbnail: string;
}

function database(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const opening = indexedDB.open("amux", 1);
    opening.onupgradeneeded = () => opening.result.createObjectStore("images", { keyPath: "id" });
    opening.onsuccess = () => resolve(opening.result);
    opening.onerror = () => reject(opening.error);
  });
}

async function images<T>(mode: IDBTransactionMode, act: (store: IDBObjectStore) => IDBRequest): Promise<T> {
  const db = await database();
  return new Promise((resolve, reject) => {
    const request = act(db.transaction("images", mode).objectStore("images"));
    request.onsuccess = () => resolve(request.result as T);
    request.onerror = () => reject(request.error);
  });
}

export function putImage(id: string, png: Uint8Array, thumbnail: string): Promise<unknown> {
  return images("readwrite", (store) => store.put({ id, png, thumbnail } satisfies Stored));
}

export async function getImage(id: string): Promise<{ png: Uint8Array; thumbnail: string } | null> {
  return (await images<Stored | undefined>("readonly", (store) => store.get(id))) ?? null;
}

export function dropImage(id: string): Promise<unknown> {
  return images("readwrite", (store) => store.delete(id));
}

/** The screenshots of a review's comments, by comment id. */
export async function reviewImages(review: PendingReview): Promise<Map<string, Image>> {
  const found = new Map<string, Image>();
  for (const comment of review.comments) {
    if (!comment.region) continue;
    const image = await getImage(comment.id);
    if (image) found.set(comment.id, { png: image.png });
  }
  return found;
}

/** Screenshots no tab's review names any more, as after the browser restarted. */
export async function sweepImages(): Promise<void> {
  const held = await chrome.storage.session.get(null);
  const named = new Set<string>();
  for (const value of Object.values(held)) {
    for (const comment of (value as PendingReview).comments ?? []) named.add(comment.id);
  }
  const ids = await images<IDBValidKey[]>("readonly", (store) => store.getAllKeys());
  for (const id of ids) if (!named.has(String(id))) await dropImage(String(id));
}
