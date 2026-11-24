import type { RenderOutcome } from '../model/render';

export const MAX_SOURCE_BYTES = 204_800;
export const SOURCE_TOO_LARGE_MESSAGE = 'The source is larger than 204,800 bytes.';

const UNKNOWN_FAILURE_MESSAGE = 'The source could not be rendered.';

/** A failure outcome for a source over the limit, or null when it fits. */
export function refuseOversizedSource(source: string): RenderOutcome | null {
  // A string never has more UTF-16 units than UTF-8 bytes, so the cheap check goes first.
  if (
    source.length <= MAX_SOURCE_BYTES &&
    new TextEncoder().encode(source).byteLength <= MAX_SOURCE_BYTES
  ) {
    return null;
  }
  return { ok: false, error: { message: SOURCE_TOO_LARGE_MESSAGE } };
}

function isPosition(n: number | undefined): n is number {
  return n !== undefined && Number.isInteger(n) && n > 0;
}

export function failure(message: string, line?: number, column?: number): RenderOutcome {
  return {
    ok: false,
    error: {
      message,
      ...(isPosition(line) ? { line } : {}),
      ...(isPosition(column) ? { column } : {}),
    },
  };
}

export function failureFrom(thrown: unknown): RenderOutcome {
  return failure(
    thrown instanceof Error && thrown.message !== '' ? thrown.message : UNKNOWN_FAILURE_MESSAGE,
  );
}

export function wrapFragment(className: string, content: DocumentFragment): DocumentFragment {
  const wrapper = document.createElement('div');
  wrapper.className = className;
  wrapper.append(content);
  const fragment = document.createDocumentFragment();
  fragment.append(wrapper);
  return fragment;
}
