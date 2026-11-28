/** A path, ID or command inside a sentence; the view renders it as inline code. */
export interface CodePart {
  readonly code: string;
}

/** Visible text: a plain string, or parts where code is marked. */
export type MarkedText = string | readonly (string | CodePart)[];

/** The text with the marks removed. */
export function plainText(text: MarkedText): string {
  if (typeof text === 'string') return text;
  return text.map((part) => (typeof part === 'string' ? part : part.code)).join('');
}
