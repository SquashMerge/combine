import { describe, expect, it, vi } from "vitest";
import { renderSource } from "../../src/renderers/lazy";

vi.mock("../../src/renderers/markdown/render", () => {
  throw new Error("chunk failed to load");
});

describe("renderSource when a renderer fails to load", () => {
  it("resolves to the not-available failure", async () => {
    await expect(renderSource("markdown", "# x")).resolves.toEqual({
      ok: false,
      error: { message: "The markdown renderer is not available." },
    });
  });
});
