export type RendererName = 'mermaid' | 'markdown';

/** `line` and `column` are 1-based and left out when unknown. */
export interface RenderError {
  message: string;
  line?: number;
  column?: number;
}

export type RenderOutcome = { ok: true; fragment: DocumentFragment } | { ok: false; error: RenderError };

export type Renderer = (source: string) => Promise<RenderOutcome>;
