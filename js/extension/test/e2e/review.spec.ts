// The extension end to end: a Chromium with it loaded, an amux daemon with
// the fake provider, and the browser starting `amux internal browser` by
// native messaging, as a person's browser does. A person comments on a
// heading, a phrase and a box on a page, sends the three to an agent from
// the side panel, and the provider receives the page review with its
// screenshot.
//
// The harness differs from a person's browser in two ways. Chromium reads
// native messaging hosts from its user data folder, so the host manifest is
// written to a temporary one and never to a real browser's. And Playwright
// cannot press the extension's toolbar button, which is what grants it the
// tab, so the copy under test is given access to every site instead.

import { type BrowserContext, type Page, type Worker, chromium, expect, test } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { chmodSync, cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, "../../../..");
const binaries = process.env.TESTNET_BINARIES ?? join(repo, "target/debug");
const built = join(here, "../../dist-dev");
const EXTENSION_ID = "omnhphmiepghpgpbfgjebeaajfppkhie";

const PAGE = `<!doctype html>
<html><head><title>Pricing</title><style>
  body { font: 16px sans-serif; margin: 0; padding: 40px; width: 900px; }
  .gap { height: 220px; margin: 24px 0; background: #f4f4f4; }
</style></head><body>
  <main>
    <h2>Monthly pricing</h2>
    <p id="renew">Our plans renew every month unless you cancel.</p>
    <div class="gap"></div>
    <div class="card" data-inspector-relative-path="src/Card.tsx" data-inspector-line="42"><p>Pro plan</p></div>
  </main>
</body></html>`;

interface Desk {
  root: string;
  config: string;
  env: NodeJS.ProcessEnv;
  inputLog: string;
}

function makeDesk(): Desk {
  const root = mkdtempSync(join(tmpdir(), "amux-browser-"));
  const bin = join(root, "bin");
  const work = join(root, "work");
  mkdirSync(bin);
  mkdirSync(work);
  writeFileSync(
    join(bin, "claude"),
    `#!/bin/sh\nfor arg in "$@"; do [ "$arg" = --print ] && exec ${join(binaries, "fake-claude-sdk")} "$@"; done\nexec ${join(binaries, "fake-claude-pty")} "$@"\n`,
  );
  chmodSync(join(bin, "claude"), 0o755);
  symlinkSync(join(binaries, "fake-codex"), join(bin, "codex"));
  const script = join(root, "script.json");
  writeFileSync(
    script,
    JSON.stringify({
      steps: [{ text: { chunks: ["ready"] } }, "turn_end", { text: { chunks: ["on it"] } }, "turn_end"],
    }),
  );
  const config = join(root, "config.toml");
  writeFileSync(
    config,
    `root = ${JSON.stringify(join(root, "data"))}\nsupervisor = false\nupdates = "manual"\nkeep_awake = false\n` +
      `releases_url = "http://127.0.0.1:9/"\n[agent]\ngrace_secs = 30\ndrain_secs = 5\n`,
  );
  const inputLog = join(root, "input.log");
  const env: NodeJS.ProcessEnv = {
    ...process.env,
    AMUX_CONFIG: config,
    PATH: `${bin}:${process.env.PATH}`,
    AMUX_FAKE_SCRIPT: script,
    AMUX_FAKE_INPUT_LOG: inputLog,
    CLAUDE_CONFIG_DIR: join(root, "claude"),
    AMUX_TEST_DISCOVERY_MODE: "disabled",
  };
  delete env.AMUX_LOG;
  return { root, config, env, inputLog };
}

function amux(desk: Desk, ...args: string[]): string {
  return execFileSync(join(binaries, "amux"), args, { env: desk.env, encoding: "utf8", timeout: 60_000 });
}

/** A browser whose native messaging host is the desk's amux. */
async function browser(desk: Desk): Promise<{ context: BrowserContext; worker: Worker }> {
  const profile = join(desk.root, "chromium");
  const extension = join(desk.root, "extension");
  cpSync(built, extension, { recursive: true });
  const manifest = JSON.parse(readFileSync(join(extension, "manifest.json"), "utf8"));
  manifest.host_permissions = ["<all_urls>"];
  writeFileSync(join(extension, "manifest.json"), JSON.stringify(manifest));

  const host = join(desk.root, "host.sh");
  const exports = ["AMUX_CONFIG", "PATH", "AMUX_FAKE_SCRIPT", "AMUX_FAKE_INPUT_LOG", "CLAUDE_CONFIG_DIR", "AMUX_TEST_DISCOVERY_MODE"]
    .map((name) => `export ${name}=${JSON.stringify(desk.env[name] ?? "")}`)
    .join("\n");
  writeFileSync(host, `#!/bin/sh\n${exports}\nexec ${JSON.stringify(join(binaries, "amux"))} internal browser "$@"\n`);
  chmodSync(host, 0o755);
  mkdirSync(join(profile, "NativeMessagingHosts"), { recursive: true });
  writeFileSync(
    join(profile, "NativeMessagingHosts", "sh.amux.browser.dev.json"),
    JSON.stringify({
      name: "sh.amux.browser.dev",
      description: "amux under test",
      path: host,
      type: "stdio",
      allowed_origins: [`chrome-extension://${EXTENSION_ID}/`],
    }),
  );

  const context = await chromium.launchPersistentContext(profile, {
    channel: "chromium",
    viewport: { width: 1000, height: 800 },
    args: [`--disable-extensions-except=${extension}`, `--load-extension=${extension}`],
  });
  const worker = context.serviceWorkers()[0] ?? (await context.waitForEvent("serviceworker"));
  return { context, worker };
}

let desk: Desk;
let server: Server;
let pageUrl: string;

test.beforeAll(async () => {
  desk = makeDesk();
  amux(desk, "server", "start");
  amux(desk, "new", "claude", "--in", "amux", "--name", "site", "--cwd", join(desk.root, "work"), "--prompt", "start");
  server = createServer((_, response) => {
    response.setHeader("content-type", "text/html");
    response.end(PAGE);
  });
  await new Promise<void>((done) => server.listen(0, "127.0.0.1", done));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("no address");
  pageUrl = `http://127.0.0.1:${address.port}/pricing`;
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

async function addComment(page: Page, words: string) {
  const editor = page.locator("amux-page-review textarea");
  await expect(editor).toBeVisible();
  await editor.fill(words);
  await page.locator("amux-page-review .save").click();
  await expect(editor).toBeHidden();
}

test("a page review goes from the page to the agent", async () => {
  const { context, worker } = await browser(desk);
  try {
    const page = await context.newPage();
    await page.goto(pageUrl);
    const tabId = await worker.evaluate(
      async (url) => (await chrome.tabs.query({ url }))[0]?.id ?? -1,
      pageUrl,
    );
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
    if (process.env.AMUX_E2E_KEEP) {
      await page.locator("amux-page-review textarea").fill("Say annual pricing");
      await page.screenshot({ path: join(here, "../../../test-results/editor.png") });
    }
    await addComment(page, "Say annual pricing");

    // A phrase: drag across "renew every month".
    const renew = await page.evaluate(() => {
      const node = document.getElementById("renew")!.firstChild!;
      const range = document.createRange();
      const start = node.textContent!.indexOf("renew");
      range.setStart(node, start);
      range.setEnd(node, start + "renew every month".length);
      const box = range.getBoundingClientRect();
      return { left: box.left + 1, right: box.right - 1, y: box.top + box.height / 2 };
    });
    await page.mouse.move(renew.left, renew.y);
    await page.mouse.down();
    await page.mouse.move(renew.right, renew.y, { steps: 8 });
    await page.mouse.up();
    await expect(page.locator("amux-page-review .editor-target")).toContainText("renew every month");
    await addComment(page, "Monthly is not true any more");

    // A box: drag over the empty grey area.
    const gap = (await page.locator(".gap").boundingBox())!;
    await page.mouse.move(gap.x + 20, gap.y + 20);
    await page.mouse.down();
    await page.mouse.move(gap.x + 300, gap.y + 160, { steps: 10 });
    await page.mouse.up();
    await expect(page.locator("amux-page-review .editor-target")).toContainText("Box");
    await addComment(page, "Too much space here");
    await expect(page.locator("amux-page-review .pin")).toHaveCount(3);
    if (process.env.AMUX_E2E_KEEP) {
      await page.locator("h2").hover();
      await page.screenshot({ path: join(here, "../../../test-results/overlay.png") });
    }

    await panel.bringToFront();
    await expect(panel.locator(".comments li")).toHaveCount(3);
    await expect(panel.locator(".thumbnail")).toHaveCount(1);
    const choice = panel.locator("select option", { hasText: "site" });
    await panel.locator("select").selectOption(await choice.getAttribute("value"));
    await panel.locator(".composer textarea").fill("Please fix these");
    if (process.env.AMUX_E2E_KEEP) await panel.screenshot({ path: join(here, "../../../test-results/panel.png") });
    await panel.getByRole("button", { name: "Send 3 comments" }).click();
    await expect(panel.locator(".outcome")).toContainText("Sent to site", { timeout: 30_000 });

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
