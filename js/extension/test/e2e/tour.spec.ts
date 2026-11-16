// A tour of every surface the extension draws, as screenshots in
// js/tour, in light and dark: the overlay's modes and comment
// box, and the side panel in each state it can reach here. It checks
// little; it is for looking at. `just browser tour` runs it.
//
// Two notices cannot be reached under Playwright: the one for the browser's
// own pages and the one for local files without file access. The panel
// learns a tab's address only once the toolbar button grants the tab, which
// Playwright cannot press. They share the not-running notice's look.

import { mkdirSync, readFileSync, rmSync } from "node:fs";
import type { Server } from "node:http";
import { join } from "node:path";
import { type BrowserContext, expect, type Page, test } from "@playwright/test";
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

test.skip(!process.env.AMUX_TOUR, "the tour runs from `just browser tour`");

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
  const served = await serve((path) => (path.startsWith("/faq") ? FAQ : path.startsWith("/doc") ? DOC : PAGE));
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
    await expect(panel.locator(".rows .row-main")).toHaveCount(3);
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
    await expect(panel.locator(".rows .row-main")).toHaveCount(4);
    await shots([panel], "panel-04-two-pages");

    await panel.locator(".picker-trigger").click();
    await expect(panel.getByRole("listbox")).toBeVisible();
    await panel.getByRole("option", { name: /^site/ }).hover();
    await shots([panel], "panel-05-picker");
    await panel.keyboard.type("work");
    await shots([panel], "panel-05-picker-search");
    await panel.keyboard.press("Escape");
    const folder = panel.getByRole("group", { name: "New agent in" }).getByRole("option");
    if ((await folder.count()) > 0) {
      await panel.locator(".picker-trigger").click();
      await folder.first().click();
      await shots([panel], "panel-05-new-agent");
    }

    await panel.locator(".picker-trigger").click();
    await panel.getByRole("option", { name: /^site/ }).click();
    await panel.locator(".composer textarea").fill("Please fix these");
    await shots([panel], "panel-06-ready");
    await panel.getByRole("button", { name: /Send 4 comments/ }).click();
    await expect(panel.locator(".banner-success")).toContainText("Sent to site", { timeout: 30_000 });
    await shots([panel], "panel-07-sent");

    await page.bringToFront();
    await shots([page], "page-09-after-send");
  } finally {
    await context.close();
  }
});

/** A page worth showing in the store: a design document an agent wrote. */
const DOC = `<!doctype html>
<html><head><meta charset="utf-8"><title>Billing redesign</title><style>
  body { margin: 0; font: 15px/1.6 -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; color: #1f2328; background: #fff; }
  main { max-width: 640px; margin: 0 auto; padding: 48px 40px; }
  .eyebrow { color: #6e7781; font-size: 13px; letter-spacing: .04em; text-transform: uppercase; }
  h1 { font-size: 30px; line-height: 1.2; margin: 26px 0 18px; }
  h2 { font-size: 19px; margin: 30px 0 8px; }
  .plans { display: grid; grid-template-columns: 1fr 1fr; gap: 14px; margin: 18px 0; }
  .plan { border: 1px solid #d0d7de; border-radius: 10px; padding: 14px 16px; }
  .plan b { display: block; font-size: 16px; }
  .plan span { color: #6e7781; font-size: 13px; }
</style></head><body><main>
  <div class="eyebrow">Design · draft 2</div>
  <h1>Billing redesign</h1>
  <p id="goal">Move every workspace to annual billing by default, with monthly kept for teams under five seats.</p>
  <h2>Plans</h2>
  <div class="plans">
    <div class="plan" id="free"><b>Free</b><span>One host, community support</span></div>
    <div class="plan" id="pro"><b>Pro</b><span>Unlimited hosts, relay included</span></div>
  </div>
  <h2>Migration</h2>
  <p id="migrate">Existing monthly subscribers switch at their next renewal, with a prorated credit for unused days.</p>
</main></body></html>`;

/**
 * The Chrome Web Store's images: two 1280×800 screenshots of the page and
 * the panel side by side, and the 440×280 promo tile.
 */
test("the store's images", async () => {
  const { context, worker } = await browser(desk, { name: "store" });
  try {
    const page = await context.newPage();
    await page.setViewportSize({ width: 880, height: 800 });
    const docUrl = `${origin}/doc`;
    await page.goto(docUrl);
    const panel = await openPanel(context, await tabOf(worker, docUrl));
    await panel.setViewportSize({ width: 400, height: 800 });
    await expect(panel.locator(".status.ok")).toBeVisible({ timeout: 30_000 });
    await panel.getByRole("button", { name: "Review this page" }).click();
    await page.bringToFront();
    await page.locator("h1").click();
    await addComment(page, "Say who this is for in the title");
    const annual = await phrase(page, "goal", "annual billing by default");
    await drag(page, { x: annual.left, y: annual.y }, { x: annual.right, y: annual.y });
    await addComment(page, "What happens to teams already on monthly?");
    const plans = (await page.locator(".plans").boundingBox())!;
    await drag(
      page,
      { x: plans.x - 6, y: plans.y - 6 },
      { x: plans.x + plans.width + 6, y: plans.y + plans.height + 6 },
    );
    await addComment(page, "Add the Team plan here");
    await page.locator("#migrate").click();
    await page.locator("amux-page-review textarea").fill("Credit or refund? Pick one.");
    await page.mouse.move(870, 790);

    await panel.locator(".picker-trigger").click();
    await panel.getByRole("option", { name: /^site/ }).click();
    await panel.locator(".composer textarea").fill("Revise the doc with these");

    const compose = async (name: string) => {
      const left = (await page.screenshot()).toString("base64");
      const right = (await panel.screenshot()).toString("base64");
      const sheet = await context.newPage();
      await sheet.setViewportSize({ width: 1280, height: 800 });
      await sheet.setContent(
        `<body style="margin:0;display:flex;background:#fff"><img src="data:image/png;base64,${left}" width="880" height="800">` +
          `<img src="data:image/png;base64,${right}" width="400" height="800" style="box-shadow:-1px 0 0 #d0d7de"></body>`,
      );
      await sheet.screenshot({ path: join(out, `${name}.png`) });
      await sheet.close();
    };
    await compose("store-1-comment");

    await page.locator("amux-page-review .save").click();
    await panel.locator(".picker-trigger").click();
    await panel.getByRole("option", { name: /^site/ }).hover();
    await compose("store-2-send");

    const icon = readFileSync(join(here, "../../static/icons/icon-128.png")).toString("base64");
    const tile = await context.newPage();
    await tile.setViewportSize({ width: 440, height: 280 });
    await tile.setContent(`<body style="margin:0;width:440px;height:280px;display:flex;flex-direction:column;justify-content:center;
      padding:0 36px;box-sizing:border-box;background:linear-gradient(135deg,#0b1220,#16233f);color:#fff;
      font:15px/1.4 -apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif">
      <img src="data:image/png;base64,${icon}" width="56" height="56" style="margin-left:-8px">
      <div style="font-size:26px;font-weight:700;letter-spacing:-.01em;margin-top:10px">Review any page</div>
      <div style="color:#b6c2d9;margin-top:6px">Comment on blocks, text and regions,<br>then send them all to your amux agent.</div></body>`);
    await tile.screenshot({ path: join(out, "store-promo-tile.png") });
    await tile.close();
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
    await expect(panel.locator(".banner")).toBeVisible();
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
    await expect(panel.locator(".banner")).toBeVisible({ timeout: 30_000 });
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
    await expect(panel.locator(".banner")).toBeVisible({ timeout: 30_000 });
    await shots([panel], "panel-10-not-running");
  } finally {
    await stopped.context.close();
  }
});
