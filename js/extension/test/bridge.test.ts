import { describe, expect, it } from "vitest";
import { joinParts, toBase64 } from "../src/background/bridge.ts";

describe("the bridge's parts", () => {
  it("joins a message sent in parts into the bytes it was", () => {
    const message = Uint8Array.from({ length: 1000 }, (_, n) => n % 251);
    const parts = [message.subarray(0, 300), message.subarray(300, 700), message.subarray(700)].map(toBase64);
    const joined = Uint8Array.from(atob(joinParts(parts)), (c) => c.charCodeAt(0));
    expect(joined).toEqual(message);
  });

  it("leaves a message in one part as it is", () => {
    expect(joinParts(["AAEC"])).toBe("AAEC");
    expect(joinParts([""])).toBe("");
  });
});

describe("the native port", () => {
  function fakeChrome() {
    const ports: Array<{ disconnected: boolean; sent: unknown[]; deliver(frame: unknown): void }> = [];
    (globalThis as { chrome?: unknown }).chrome = {
      runtime: {
        lastError: undefined,
        connectNative() {
          const listeners: Array<(frame: unknown) => void> = [];
          const port = {
            disconnected: false,
            sent: [] as unknown[],
            deliver: (frame: unknown) => {
              for (const listen of listeners) listen(frame);
            },
            onMessage: { addListener: (listen: (frame: unknown) => void) => listeners.push(listen) },
            onDisconnect: { addListener() {} },
            postMessage(message: unknown) {
              port.sent.push(message);
            },
            disconnect() {
              port.disconnected = true;
            },
          };
          ports.push(port);
          return port;
        },
      },
    };
    return ports;
  }

  it("closes once no call is open and nothing holds it, so the service worker can stop", async () => {
    const ports = fakeChrome();
    const { Bridge } = await import("../src/background/bridge.ts");
    const bridge = new Bridge("sh.amux.browser");
    const answered = bridge.unary("installation", "amux.local", "/amux.v1.InstallationService/ListProfiles", "");
    ports[0]!.deliver({ id: 1, message: "" });
    ports[0]!.deliver({ id: 1, end: { code: 0, message: "", details: "" } });
    await answered;
    expect(ports[0]!.disconnected).toBe(true);
  });

  it("stays open between calls while held, and closes when let go", async () => {
    const ports = fakeChrome();
    const { Bridge } = await import("../src/background/bridge.ts");
    const bridge = new Bridge("sh.amux.browser");
    bridge.hold(true);
    const cancel = bridge.stream(
      "installation",
      "amux.local",
      "/amux.v1.InventoryService/SubscribeInventory",
      "",
      () => {},
      () => {},
    );
    cancel();
    expect(ports[0]!.sent).toContainEqual({ id: 1, cancel: true });
    expect(ports[0]!.disconnected).toBe(false);
    bridge.hold(false);
    expect(ports[0]!.disconnected).toBe(true);
  });

  it("sends every call at once with the authority it names", async () => {
    const ports = fakeChrome();
    const { Bridge, NODE } = await import("../src/background/bridge.ts");
    const bridge = new Bridge("sh.amux.browser");
    bridge.hold(true);
    bridge.stream(
      "installation",
      NODE,
      "/amux.v1.InstallationService/ListProfiles",
      "",
      () => {},
      () => {},
    );
    bridge.stream(
      "ab01",
      "far",
      "/amux.v1.AgentService/SendInput",
      "AA==",
      () => {},
      () => {},
    );
    expect(ports[0]!.sent).toEqual([
      {
        id: 1,
        call: {
          target: "installation",
          method: "/amux.v1.InstallationService/ListProfiles",
          body: "",
          authority: NODE,
        },
      },
      { id: 2, call: { target: "ab01", method: "/amux.v1.AgentService/SendInput", body: "AA==", authority: "far" } },
    ]);
  });
});
