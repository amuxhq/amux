// The extension end to end: a Chromium with it loaded, an amux daemon with
// the fake provider, and the browser starting `amux internal browser` by
// native messaging, as a person's browser does. A person comments on a
// heading, a phrase and a box on a page, sends the three to an agent from
// the side panel, and the provider receives the page review with its
// screenshot. See harness.ts for how this differs from a person's browser.

import { expect, test } from "@playwright/test";
import { readFileSync, rmSync, writeFileSync } from "node:fs";
import type { Server } from "node:http";
import { join } from "node:path";
import {
  addComment,
  amux,
  browser,
  type Desk,
  drag,
  EXTENSION_ID,
  here,
  makeDesk,
  PAGE,
  phrase,
  serve,
  tabOf,
} from "./harness.ts";

let desk: Desk;
let server: Server;
let pageUrl: string;

test.beforeAll(async () => {
  desk = makeDesk();
  amux(desk, "server", "start");
  amux(desk, "new", "claude", "--in", "amux", "--name", "site", "--cwd", join(desk.root, "work"), "--prompt", "start");
  const served = await serve(() => PAGE);
  server = served.server;
  pageUrl = `${served.origin}/pricing`;
});

test.afterAll(async () => {
  server?.close();
  try {
    amux(desk, "server", "stop");
  } catch {
    // Already gone.
  }
  rmSync(desk.root, { recursive: true, force: true });
});

test("a page review goes from the page to the agent", async () => {
  const { context, worker } = await browser(desk);
  try {
    const page = await context.newPage();
    await page.goto(pageUrl);
    const tabId = await tabOf(worker, pageUrl);
    expect(tabId).toBeGreaterThan(0);

    const panel = await context.newPage();
    await panel.goto(`chrome-extension://${EXTENSION_ID}/panel.html?tab=${tabId}`);
    await expect(panel.locator(".status.ok")).toBeVisible({ timeout: 30_000 });
    await panel.getByRole("button", { name: "Review this page" }).click();

    await page.bringToFront();
    await expect(page.locator("amux-page-review .toolbar")).toBeVisible();

    // A block: the heading.
    await page.locator("h2").click();
    await expect(page.locator("amux-page-review .editor-target")).toContainText("h2");
    await addComment(page, "Say annual pricing");

    // A phrase: drag across "renew every month".
    const renew = await phrase(page, "renew", "renew every month");
    await drag(page, { x: renew.left, y: renew.y }, { x: renew.right, y: renew.y });
    await expect(page.locator("amux-page-review .editor-target")).toContainText("renew every month");
    await addComment(page, "Monthly is not true any more");

    // A box: drag over the empty grey area.
    const gap = (await page.locator(".gap").boundingBox())!;
    await drag(page, { x: gap.x + 20, y: gap.y + 20 }, { x: gap.x + 300, y: gap.y + 160 });
    await expect(page.locator("amux-page-review .editor-target")).toContainText("Box");
    await addComment(page, "Too much space here");
    await expect(page.locator("amux-page-review .pin")).toHaveCount(3);

    await panel.bringToFront();
    await expect(panel.locator(".rows .row")).toHaveCount(3);
    await expect(panel.locator(".thumbnail")).toHaveCount(1);
    await panel.locator(".picker-trigger").click();
    await panel.getByRole("option", { name: /^site/ }).click();
    await panel.locator(".composer textarea").fill("Please fix these");
    await panel.getByRole("button", { name: "Send 3 comments" }).click();
    await expect(panel.locator(".banner-success")).toContainText("Sent to site", { timeout: 30_000 });

    // What the provider read.
    await expect
      .poll(() => {
        try {
          return readFileSync(desk.inputLog, "utf8");
        } catch {
          return "";
        }
      }, { timeout: 30_000 })
      .toContain("Too much space here");
    const read = readFileSync(desk.inputLog, "utf8");
    expect(read).toContain("Please fix these");
    expect(read).toContain('kind=\\"page\\"');
    expect(read).toContain(`url=\\"${pageUrl}\\"`);
    expect(read).toContain("text: Say annual pricing");
    expect(read).toContain("quote: renew every month");
    expect(read).toContain("image: region-3.png");
    expect(read).toContain('"type":"image"');
    if (process.env.AMUX_E2E_KEEP) writeFileSync(join(here, "../../../test-results/input.log"), read);
  } finally {
    await context.close();
  }
});
