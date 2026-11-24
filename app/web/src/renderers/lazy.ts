import type { RenderOutcome, Renderer, RendererName } from "../model/render";
import { failure, failureFrom } from "./outcome";

const NOT_AVAILABLE: Record<RendererName, string> = {
  mermaid: "The mermaid renderer is not available.",
  markdown: "The markdown renderer is not available.",
};

/**
 * The one place the renderers are loaded, and what the view panel calls. Each renderer module
 * and its libraries stay out of the entry chunk. Never rejects.
 */
export async function renderSource(
  renderer: RendererName,
  source: string,
): Promise<RenderOutcome> {
  let render: Renderer;
  try {
    render =
      renderer === "mermaid"
        ? (await import("./mermaid/render")).renderMermaid
        : (await import("./markdown/render")).renderMarkdown;
  } catch {
    return failure(NOT_AVAILABLE[renderer]);
  }
  try {
    return await render(source);
  } catch (e) {
    return failureFrom(e);
  }
}
