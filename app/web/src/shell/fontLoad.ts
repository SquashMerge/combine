import { BUNDLED_FONT } from '../model/tokens';
import { TERMINAL_FONT } from '../renderers/terminal/settings';

export type FontLoadOutcome = 'loaded' | 'missing' | 'failed' | 'timeout' | 'unsupported';

export const FONT_LOAD_TIMEOUT_MS = 3000;

export const FONT_LOAD_SPECS = [
  `400 ${TERMINAL_FONT.size}px '${BUNDLED_FONT.mono}'`,
  `700 ${TERMINAL_FONT.size}px '${BUNDLED_FONT.mono}'`,
  `400 14px '${BUNDLED_FONT.ui}'`,
  `600 14px '${BUNDLED_FONT.ui}'`,
  `700 18px '${BUNDLED_FONT.display}'`,
] as const;

type Fonts = Pick<FontFaceSet, 'load' | 'check'>;

/** Resolves once the bundled faces are loaded or the timeout fires; never rejects. */
export async function waitForFonts(
  fonts: Fonts | undefined,
  timeoutMs: number = FONT_LOAD_TIMEOUT_MS,
): Promise<FontLoadOutcome> {
  if (fonts === undefined) return 'unsupported';
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<FontLoadOutcome>((resolve) => {
    timer = setTimeout(() => resolve('timeout'), timeoutMs);
  });
  const loads = (async (): Promise<FontLoadOutcome> => {
    try {
      const loaded = await Promise.all(FONT_LOAD_SPECS.map((spec) => fonts.load(spec)));
      const present = loaded.every((faces) => faces.length > 0);
      return present && FONT_LOAD_SPECS.every((spec) => fonts.check(spec)) ? 'loaded' : 'missing';
    } catch {
      return 'failed';
    }
  })();
  try {
    return await Promise.race([loads, timeout]);
  } finally {
    clearTimeout(timer);
  }
}

/** Calls `render` exactly once after the fonts settle, whatever the outcome. */
export async function renderAfterFonts(
  fonts: Fonts | undefined,
  render: () => void,
  timeoutMs?: number,
): Promise<FontLoadOutcome> {
  const outcome = await waitForFonts(fonts, timeoutMs);
  render();
  return outcome;
}
