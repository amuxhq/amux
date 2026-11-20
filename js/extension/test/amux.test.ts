import { beforeEach, describe, expect, it, vi } from "vitest";

// browser-core stands in as its answers: what the extension sends is what is
// under test, not the encodings.
vi.mock("../src/background/core.ts", () => ({
  core: async (request: { op: string } & Record<string, unknown>) => {
    switch (request.op) {
      case "profiles":
        return [{ id: "ab01", label: "work" }];
      case "deviceIdentity":
        return { hostId: LOCAL };
      case "fleetNew":
        return 1;
      case "folders":
      case "picker":
        return [];
      case "authority":
        return `authority of ${request.host}`;
      case "verdict":
        return { accepted: true };
      case "blob":
        return { hash: "", name: "region-1.png", mime: "image/png", size: 1 };
      case "agentKind":
        return "KIND_CLAUDE_SDK";
      default:
        return `${request.op} body`;
    }
  },
}));

const LOCAL = "11111111111111111111111111111111";
const FAR = "22222222222222222222222222222222";

interface Sent {
  id: number;
  call?: { target: string; method: string; body: string; authority: string };
}

/** A native port to a bridge of `framing` that answers every unary call. */
function fakeBridge(framing: number): Sent[] {
  const sent: Sent[] = [];
  (globalThis as { chrome?: unknown }).chrome = {
    runtime: {
      lastError: undefined,
      connectNative() {
        const listeners: Array<(frame: unknown) => void> = [];
        const deliver = (frame: unknown) =>
          queueMicrotask(() => {
            for (const listen of listeners) listen(frame);
          });
        deliver({ hello: { version: "0.12.0", framing } });
        return {
          onMessage: { addListener: (listen: (frame: unknown) => void) => listeners.push(listen) },
          onDisconnect: { addListener() {} },
          postMessage(message: Sent) {
            sent.push(message);
            if (!message.call || message.call.method.endsWith("/SubscribeInventory")) return;
            deliver({ id: message.id, message: "" });
            deliver({ id: message.id, end: { code: 0, message: "", details: "" } });
          },
          disconnect() {},
        };
      },
    },
  };
  return sent;
}

const review = {
  url: "http://localhost:5173/",
  title: "Home",
  comments: [
    {
      id: "c1",
      url: "http://localhost:5173/",
      title: "Home",
      element: { selector: "h1", text: "Welcome", source: "" },
      quote: "",
      region: null,
      text: "Bigger",
      pin: { x: 0, y: 0 },
    },
  ],
  snippets: [],
};

async function sendToFarAgent(framing: number): Promise<Sent[]> {
  const sent = fakeBridge(framing);
  const { Amux } = await import("../src/background/amux.ts");
  const amux = new Amux("sh.amux.browser");
  await amux.watch();
  await amux.send(
    { kind: "agent", profile: "ab01", host: FAR, agent: "aa" },
    review,
    "Fix it",
    new Map([["c1", { png: new Uint8Array([1]) }]]),
    () => {},
  );
  amux.unwatch();
  return sent;
}

describe("what the extension asks amux", () => {
  beforeEach(() => {
    vi.resetModules();
  });

  it("names the agent's host as the authority of every act on it", async () => {
    const calls = (await sendToFarAgent(2)).flatMap((frame) => (frame.call ? [frame.call] : []));
    const acts = calls.filter((call) => call.method.startsWith("/amux.v1.AgentService/"));
    expect(acts.map((call) => [call.method, call.authority])).toEqual([
      ["/amux.v1.AgentService/PutBlob", `authority of ${FAR}`],
      ["/amux.v1.AgentService/SendInput", `authority of ${FAR}`],
    ]);
    // Reads of the profile's own node name the node.
    expect(calls.filter((call) => !acts.includes(call)).map((call) => [call.method, call.authority])).toEqual([
      ["/amux.v1.InstallationService/ListProfiles", "amux.local"],
      ["/amux.v1.ProfileAdminService/GetDeviceIdentity", "amux.local"],
      ["/amux.v1.InventoryService/SubscribeInventory", "amux.local"],
    ]);
  });

  it("says amux needs updating when its bridge is older than this extension", async () => {
    fakeBridge(1);
    const { Amux } = await import("../src/background/amux.ts");
    const amux = new Amux("sh.amux.browser");
    await amux.watch();
    expect(amux.reach()).toEqual({ status: "not-installed" });
    amux.unwatch();
  });

  it("makes a new agent on the profile's own host, addressed to it", async () => {
    const sent = fakeBridge(2);
    const { Amux } = await import("../src/background/amux.ts");
    const amux = new Amux("sh.amux.browser");
    await amux.watch();
    await amux.send({ kind: "new", profile: "ab01", cwd: "~/site" }, review, "Fix it", new Map(), () => {});
    amux.unwatch();
    const calls = sent.flatMap((frame) => (frame.call ? [frame.call] : []));
    expect(
      calls
        .filter((call) => call.method.startsWith("/amux.v1.AgentService/"))
        .map((call) => [call.method, call.authority]),
    ).toEqual([
      ["/amux.v1.AgentService/CreateAgent", `authority of ${LOCAL}`],
      ["/amux.v1.AgentService/SendInput", `authority of ${LOCAL}`],
    ]);
  });
});
