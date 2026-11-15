import { describe, expect, it } from "vitest";
import { byPage, type PendingComment, pageOf } from "../src/shared/messages.ts";

function comment(id: string, url: string): PendingComment {
  return {
    id,
    url,
    title: url,
    element: { selector: "h1", text: "", source: "" },
    quote: "",
    region: null,
    text: id,
    pin: { x: 0, y: 0 },
  };
}

describe("a tab's comments by page", () => {
  it("keeps each comment with the page it was made on, pages in the order first commented", () => {
    const pages = byPage([comment("a", "/one"), comment("b", "/two"), comment("c", "/one")]);
    expect(pages.map((page) => [page.url, page.comments.map((c) => c.id)])).toEqual([
      ["/one", ["a", "c"]],
      ["/two", ["b"]],
    ]);
  });

  it("takes a link within a document for the same page", () => {
    expect(pageOf("file:///doc.html#section")).toBe("file:///doc.html");
    expect(pageOf("http://localhost:5173/pricing")).toBe("http://localhost:5173/pricing");
    expect(pageOf(undefined)).toBe("");
    expect(pageOf("http://localhost:5173/#/settings")).toBe("http://localhost:5173/#/settings");
    expect(pageOf("http://localhost:5173/#!/settings")).toBe("http://localhost:5173/#!/settings");
  });
});
