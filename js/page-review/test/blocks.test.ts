import { beforeEach, describe, expect, it } from "vitest";
import { anchorOf, selectorFor, snap, sourceFromMarkup, visibleText, wider } from "../src/blocks.ts";

beforeEach(() => {
  document.body.innerHTML = `
    <main>
      <section id="pricing">
        <h2>Monthly <em>pricing</em></h2>
        <div class="cards">
          <div class="card"><p>Starter <a href="#">plan</a></p></div>
          <div class="card" data-inspector-relative-path="src/Card.tsx" data-inspector-line="42"><p><span>Pro</span> plan</p></div>
        </div>
      </section>
      <p data-source="src/Footer.tsx:7:3">Footer</p>
    </main>`;
});

describe("snapping", () => {
  it("climbs from an inline run to its block", () => {
    const em = document.querySelector("em")!;
    expect(snap(em).tagName).toBe("H2");
    const link = document.querySelector("a")!;
    expect(snap(link).tagName).toBe("P");
  });

  it("widens to the block around", () => {
    const paragraph = document.querySelectorAll(".card p")[1]!;
    const card = wider(paragraph)!;
    expect(card.classList.contains("card")).toBe(true);
    expect(wider(card)!.classList.contains("cards")).toBe(true);
  });
});

describe("anchors", () => {
  it("names a block by a selector that finds it alone", () => {
    const paragraphs = document.querySelectorAll(".card p");
    for (const paragraph of paragraphs) {
      const selector = selectorFor(paragraph);
      expect(document.querySelectorAll(selector)).toHaveLength(1);
      expect(document.querySelector(selector)).toBe(paragraph);
    }
    expect(selectorFor(document.querySelector("h2")!)).toBe("#pricing > h2");
  });

  it("keeps visible text short and folded", () => {
    const long = document.createElement("p");
    long.textContent = `${"word ".repeat(100)}\n\n end`;
    document.body.append(long);
    const text = visibleText(long);
    expect(text.length).toBeLessThanOrEqual(200);
    expect(text.endsWith("…")).toBe(true);
    expect(text).not.toMatch(/\s\s/);
  });

  it("reads where development tools say a block was drawn from", () => {
    const pro = document.querySelector("span")!;
    expect(sourceFromMarkup(pro)).toBe("src/Card.tsx:42");
    const footer = document.querySelector("[data-source]")!;
    expect(sourceFromMarkup(footer)).toBe("src/Footer.tsx:7");
    expect(sourceFromMarkup(document.querySelector("h2")!)).toBe("");
    expect(anchorOf(document.querySelector("h2")!, "src/Pricing.tsx:3").source).toBe("src/Pricing.tsx:3");
  });
});
