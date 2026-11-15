// A dragged box as a screenshot: the visible tab, cropped to the box.

import type { Region } from "../shared/messages.ts";

/** The widest a thumbnail in the side panel is drawn, in pixels. */
const THUMBNAIL_WIDTH = 240;

export interface Shot {
  png: Uint8Array;
  /** A small JPEG of it, as a data URL, for the side panel. */
  thumbnail: string;
}

/** Crops a capture of the visible tab to `region`. */
export async function crop(captured: Blob, region: Region): Promise<Shot> {
  const bitmap = await createImageBitmap(captured);
  const scale = region.devicePixelRatio || 1;
  const x = Math.max(0, Math.round(region.x * scale));
  const y = Math.max(0, Math.round(region.y * scale));
  const width = Math.max(1, Math.min(bitmap.width - x, Math.round(region.width * scale)));
  const height = Math.max(1, Math.min(bitmap.height - y, Math.round(region.height * scale)));
  const canvas = new OffscreenCanvas(width, height);
  canvas.getContext("2d")!.drawImage(bitmap, x, y, width, height, 0, 0, width, height);
  const png = new Uint8Array(await (await canvas.convertToBlob({ type: "image/png" })).arrayBuffer());

  const thumbScale = Math.min(1, THUMBNAIL_WIDTH / width);
  const small = new OffscreenCanvas(
    Math.max(1, Math.round(width * thumbScale)),
    Math.max(1, Math.round(height * thumbScale)),
  );
  small.getContext("2d")!.drawImage(canvas, 0, 0, small.width, small.height);
  const jpeg = new Uint8Array(await (await small.convertToBlob({ type: "image/jpeg", quality: 0.8 })).arrayBuffer());
  let binary = "";
  for (let at = 0; at < jpeg.length; at += 0x8000) binary += String.fromCharCode(...jpeg.subarray(at, at + 0x8000));
  return { png, thumbnail: `data:image/jpeg;base64,${btoa(binary)}` };
}

/** Captures the visible part of a window's active tab and crops it. */
export async function captureRegion(windowId: number, region: Region): Promise<Shot> {
  const dataUrl = await chrome.tabs.captureVisibleTab(windowId, { format: "png" });
  const captured = await (await fetch(dataUrl)).blob();
  return crop(captured, region);
}
