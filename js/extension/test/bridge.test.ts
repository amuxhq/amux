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
