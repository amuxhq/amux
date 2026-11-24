// The harness the end-to-end tests share: a temporary amux with the fake
// providers, and a Chromium with the development extension loaded whose
// native messaging host is that amux.
//
// It differs from a person's browser in two ways. Chromium reads native
// messaging hosts from its user data folder, so the host manifest is written
// to a temporary one and never to a real browser's. And Playwright cannot
// press the extension's toolbar button, which is what grants it a tab, so
// the copy under test is given access to every site unless a test asks for
// the grant a person's browser would have.

import { execFileSync } from "node:child_process";
import { chmodSync, cpSync, mkdirSync, mkdtempSync, readFileSync, symlinkSync, writeFileSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { type BrowserContext, chromium, expect, type Page, type Worker } from "@playwright/test";

export const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, "../../../..");
const binaries = process.env.TESTNET_BINARIES ?? join(repo, "target/debug");
// Buck2's test (js/BUCK) hands it the extension it built and the Linux
// toolchain's Chromium; `just browser cargo-test` builds into dist-dev and
// uses the Chromium Playwright installs.
const built = process.env.AMUX_EXTENSION ?? join(here, "../../dist-dev");
const chromiumPath = process.env.AMUX_CHROMIUM;
export const EXTENSION_ID = "omnhphmiepghpgpbfgjebeaajfppkhie";

export const PAGE = `<!doctype html>
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

export interface Desk {
  root: string;
  config: string;
  env: NodeJS.ProcessEnv;
  inputLog: string;
}

export function makeDesk(): Desk {
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
      steps: [
        { text: { chunks: ["ready"] } },
        "turn_end",
        ...Array(12)
          .fill([{ text: { chunks: ["on it"] } }, "turn_end"])
          .flat(),
      ],
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

export function amux(desk: Desk, ...args: string[]): string {
  return execFileSync(join(binaries, "amux"), args, { env: desk.env, encoding: "utf8", timeout: 60_000 });
}

export interface Options {
  /** Give the extension every site, standing in for the toolbar button's grant. Default true. */
  allSites?: boolean;
  /** Register the native messaging host. Default true. */
  host?: boolean;
  /** A name for this browser's folders, so one desk can run several. */
  name?: string;
}

/** A browser whose native messaging host is the desk's amux. */
export async function browser(desk: Desk, options: Options = {}): Promise<{ context: BrowserContext; worker: Worker }> {
  const name = options.name ?? "chromium";
  const profile = join(desk.root, name);
  const extension = join(desk.root, `extension-${name}`);
  cpSync(built, extension, { recursive: true });
  // The build may be read-only (Buck2's outputs are); the copy is this test's.
  execFileSync("chmod", ["-R", "u+w", extension]);
  if (options.allSites ?? true) {
    const manifest = JSON.parse(readFileSync(join(extension, "manifest.json"), "utf8"));
    manifest.host_permissions = ["<all_urls>"];
    writeFileSync(join(extension, "manifest.json"), JSON.stringify(manifest));
  }
  if (options.host ?? true) writeHost(desk, profile);
  const context = await chromium.launchPersistentContext(profile, {
    ...(chromiumPath ? { executablePath: chromiumPath } : { channel: "chromium" }),
    viewport: { width: 1000, height: 800 },
    args: [`--disable-extensions-except=${extension}`, `--load-extension=${extension}`],
  });
  const worker = context.serviceWorkers()[0] ?? (await context.waitForEvent("serviceworker"));
  return { context, worker };
}

function writeHost(desk: Desk, profile: string) {
  const host = join(desk.root, "host.sh");
  const exports = [
    "AMUX_CONFIG",
    "PATH",
    "AMUX_FAKE_SCRIPT",
    "AMUX_FAKE_INPUT_LOG",
    "CLAUDE_CONFIG_DIR",
    "AMUX_TEST_DISCOVERY_MODE",
  ]
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
}

/** A local web server answering every path with `html`. */
export async function serve(html: (path: string) => string): Promise<{ server: Server; origin: string }> {
  const server = createServer((request, response) => {
    response.setHeader("content-type", "text/html");
    response.end(html(request.url ?? "/"));
  });
  await new Promise<void>((done) => server.listen(0, "127.0.0.1", done));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("no address");
  return { server, origin: `http://127.0.0.1:${address.port}` };
}

/** The id of the tab showing `url`. */
export async function tabOf(worker: Worker, url: string): Promise<number> {
  return worker.evaluate(async (url) => (await chrome.tabs.query({ url }))[0]?.id ?? -1, url);
}

/** Writes a comment into the open comment box and adds it. */
export async function addComment(page: Page, words: string) {
  const editor = page.locator("amux-page-review textarea");
  await expect(editor).toBeVisible();
  await editor.fill(words);
  await page.locator("amux-page-review .save").click();
  await expect(editor).toBeHidden();
}

/** Drags from one point to another, as a person selects text or draws a box. */
export async function drag(page: Page, from: { x: number; y: number }, to: { x: number; y: number }, release = true) {
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.mouse.move(to.x, to.y, { steps: 10 });
  if (release) await page.mouse.up();
}

/** Where a phrase of an element's text sits, for dragging across it. */
export async function phrase(page: Page, id: string, words: string) {
  return page.evaluate(
    ({ id, words }) => {
      const node = document.getElementById(id)!.firstChild!;
      const range = document.createRange();
      const start = node.textContent!.indexOf(words);
      range.setStart(node, start);
      range.setEnd(node, start + words.length);
      const box = range.getBoundingClientRect();
      return { left: box.left + 1, right: box.right - 1, y: box.top + box.height / 2 };
    },
    { id, words },
  );
}
