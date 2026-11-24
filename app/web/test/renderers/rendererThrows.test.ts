import { describe, expect, it, vi } from "vitest";
import { renderSource } from "../../src/renderers/lazy";

vi.mock("../../src/renderers/markdown/render", () => ({
  renderMarkdown: () => {
    throw new Error("boom");
  },
}));
vi.mock("../../src/renderers/mermaid/render", () => ({
  renderMermaid: () => Promise.reject(new Error("late")),
}));

describe("renderSource when a renderer throws", () => {
  it("resolves a synchronous throw to a failure", async () => {
    await expect(renderSource("markdown", "# x")).resolves.toEqual({
      ok: false,
      error: { message: "boom" },
    });
  });

  it("resolves a rejected promise to a failure", async () => {
    await expect(renderSource("mermaid", "x")).resolves.toEqual({
      ok: false,
      error: { message: "late" },
    });
  });
});
