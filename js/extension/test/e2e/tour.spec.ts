// A tour of every surface the extension draws, as screenshots in
// js/tour, in light and dark: the overlay's modes and comment
// box, and the side panel in each state it can reach here. It checks
// little; it is for looking at. `just browser-tour` runs it.
//
// Two notices cannot be reached under Playwright: the one for the browser's
// own pages and the one for local files without file access. The panel
// learns a tab's address only once the toolbar button grants the tab, which
// Playwright cannot press. They share the not-running notice's look.

import { type BrowserContext, type Page, expect, test } from "@playwright/test";
import { mkdirSync, rmSync } from "node:fs";
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

test.skip(!process.env.AMUX_TOUR, "the tour runs from `just browser-tour`");

const out = join(here, "../../../tour");
const PANEL = { width: 400, height: 760 };

const FAQ = `<!doctype html>
<html><head><title>Questions</title><style>
  body { font: 16px sans-serif; margin: 0; padding: 40px; width: 900px; }
</style></head><body>
  <main>
    <h2>Questions</h2>
    <p id="refunds">Refunds are given within 30 days of purchase.</p>
  </main>
</body></html>`;

let desk: Desk;
let server: Server;
let origin: string;

test.beforeAll(async () => {
  mkdirSync(out, { recursive: true });
  desk = makeDesk();
  amux(desk, "server", "start");
  amux(desk, "new", "claude", "--in", "amux", "--name", "site", "--cwd", join(desk.root, "work"), "--prompt", "start");
  amux(desk, "new", "claude", "--in", "amux", "--name", "docs", "--cwd", join(desk.root, "work"), "--prompt", "start");
  const served = await serve((path) => (path.startsWith("/faq") ? FAQ : PAGE));
  server = served.server;
  origin = served.origin;
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

async function shot(page: Page, name: string) {
  await page.waitForTimeout(150);
  await page.screenshot({ path: join(out, `${name}.png`) });
}

/** Both colour schemes of the same moment. */
async function shots(pages: Page[], name: string) {
  for (const scheme of ["light", "dark"] as const) {
    for (const page of pages) await page.emulateMedia({ colorScheme: scheme });
    await shot(pages[pages.length - 1]!, `${name}-${scheme}`);
  }
  for (const page of pages) await page.emulateMedia({ colorScheme: "light" });
}

async function openPanel(context: BrowserContext, tabId: number): Promise<Page> {
  const panel = await context.newPage();
  await panel.setViewportSize(PANEL);
  await panel.goto(`chrome-extension://${EXTENSION_ID}/panel.html?tab=${tabId}`);
  return panel;
}

test("the overlay and the panel, comment by comment", async () => {
  const { context, worker } = await browser(desk);
  try {
    const page = await context.newPage();
    const pricing = `${origin}/pricing`;
    await page.goto(pricing);
    const tabId = await tabOf(worker, pricing);
    const panel = await openPanel(context, tabId);
    await expect(panel.locator(".status.ok")).toBeVisible({ timeout: 30_000 });
    await shots([panel], "panel-01-start");

    await panel.getByRole("button", { name: "Review this page" }).click();
    await expect(panel.locator(".empty")).toContainText("Click a block");
    await shots([panel], "panel-02-reviewing");

    await page.bringToFront();
    await expect(page.locator("amux-page-review .toolbar")).toBeVisible();
    await page.locator("#renew").hover();
    await shots([page], "page-01-hover");

    await page.locator("h2").click();
    await page.locator("amux-page-review textarea").fill("Say annual pricing");
    await shots([page], "page-02-editor-block");
    await page.locator("amux-page-review .save").click();

    const renew = await phrase(page, "renew", "renew every month");
    await drag(page, { x: renew.left, y: renew.y }, { x: renew.right, y: renew.y });
    await page.locator("amux-page-review textarea").fill("Monthly is not true any more");
    await shots([page], "page-03-editor-quote");
    await page.locator("amux-page-review .save").click();

    const gap = (await page.locator(".gap").boundingBox())!;
    await drag(page, { x: gap.x + 20, y: gap.y + 20 }, { x: gap.x + 300, y: gap.y + 160 }, false);
    await shots([page], "page-04-drawing-box");
    await page.mouse.up();
    await expect(page.locator("amux-page-review .editor-target")).toContainText("Box");
    await page.locator("amux-page-review textarea").fill("Too much space here");
    await shots([page], "page-05-editor-box");
    await page.locator("amux-page-review .save").click();

    await page.locator("amux-page-review .tool-box").click();
    await page.locator(".card").hover();
    await shots([page], "page-06-box-tool");
    await page.locator("amux-page-review .tool-box").click();

    await page.mouse.move(900, 700);
    await shots([page], "page-07-pins");

    await page.locator("amux-page-review .pin").first().click();
    await shots([page], "page-08-editing");
    await page.locator("amux-page-review .cancel").click();

    await panel.bringToFront();
    await expect(panel.locator(".comments .comment")).toHaveCount(3);
    await shots([panel], "panel-03-comments");

    // Follow a link in the same tab and comment there too.
    await page.bringToFront();
    await page.goto(`${origin}/faq`);
    await panel.bringToFront();
    await panel.getByRole("button", { name: "Add more on the page" }).click();
    await page.bringToFront();
    await page.locator("#refunds").click();
    await addComment(page, "Say 60 days");
    await panel.bringToFront();
    await expect(panel.locator(".comments .comment")).toHaveCount(4);
    await shots([panel], "panel-04-two-pages");

    const folder = panel.locator("select option", { hasText: "work" }).last();
    const value = await folder.getAttribute("value");
    if (value?.includes('"new"')) {
      await panel.locator("select").selectOption(value);
      await shots([panel], "panel-05-new-agent");
    }

    const site = panel.locator("select option", { hasText: "site" });
    await panel.locator("select").selectOption(await site.getAttribute("value"));
    await panel.locator(".composer textarea").fill("Please fix these");
    await shots([panel], "panel-06-ready");
    await panel.getByRole("button", { name: /Send 4 comments/ }).click();
    await expect(panel.locator(".outcome")).toContainText("Sent to site", { timeout: 30_000 });
    await shots([panel], "panel-07-sent");

    await page.bringToFront();
    await shots([page], "page-09-after-send");
  } finally {
    await context.close();
  }
});

test("the panel where the extension has not been let in", async () => {
  const { context, worker } = await browser(desk, { allSites: false, name: "ungranted" });
  try {
    const page = await context.newPage();
    const pricing = `${origin}/pricing`;
    await page.goto(pricing);
    const tabs = await worker.evaluate(async () => (await chrome.tabs.query({})).map((tab) => tab.id ?? -1));
    const tabId = tabs[tabs.length - 1]!;
    const panel = await openPanel(context, tabId);
    await expect(panel.locator(".status.ok")).toBeVisible({ timeout: 30_000 });
    await panel.getByRole("button", { name: "Review this page" }).click();
    await expect(panel.locator(".notice")).toBeVisible();
    await shots([panel], "panel-08-needs-grant");
  } finally {
    await context.close();
  }
});

test("the panel when amux is not installed, and not running", async () => {
  const missing = await browser(desk, { host: false, name: "no-host" });
  try {
    const page = await missing.context.newPage();
    await page.goto(`${origin}/pricing`);
    const panel = await openPanel(missing.context, await tabOf(missing.worker, `${origin}/pricing`));
    await expect(panel.locator(".notice")).toBeVisible({ timeout: 30_000 });
    await shots([panel], "panel-09-not-installed");
  } finally {
    await missing.context.close();
  }

  amux(desk, "server", "stop");
  const stopped = await browser(desk, { name: "stopped" });
  try {
    const page = await stopped.context.newPage();
    await page.goto(`${origin}/pricing`);
    const panel = await openPanel(stopped.context, await tabOf(stopped.worker, `${origin}/pricing`));
    await expect(panel.locator(".notice")).toBeVisible({ timeout: 30_000 });
    await shots([panel], "panel-10-not-running");
  } finally {
    await stopped.context.close();
  }
});
