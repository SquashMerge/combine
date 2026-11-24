import { readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { beforeEach, describe, expect, it } from "vitest";
import { renderSource } from "../../src/renderers/lazy";
import { stubCanvas } from "../support/canvas";

const classSource = readFileSync(
  join(resolve(import.meta.dirname, "../fixtures/renderers"), "class.mmd"),
  "utf8",
);

beforeEach(() => {
  stubCanvas();
});

describe("renderSource", () => {
  it("renders a mermaid source", async () => {
    const outcome = await renderSource("mermaid", classSource);
    expect(outcome.ok).toBe(true);
  });

  it("renders a markdown source", async () => {
    const outcome = await renderSource("markdown", "# x");
    expect(outcome.ok).toBe(true);
  });
});
