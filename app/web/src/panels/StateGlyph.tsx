import type { PanelStatus } from './attachmentMachine';
import './stateGlyph.css';

export type GlyphShape =
  | 'filled-dot'
  | 'half-ring'
  | 'ring'
  | 'square'
  | 'dashed-ring'
  | 'cross'
  | 'check';

/** Shape for a terminal's state word; `null` for a state with no glyph. */
export function terminalStateShape(state: string): GlyphShape | null {
  switch (state) {
    case 'running':
      return 'filled-dot';
    case 'waiting':
      return 'half-ring';
    case 'idle':
      return 'ring';
    case 'exited':
      return 'square';
    case 'unknown':
      return 'dashed-ring';
    case 'gone':
      return 'cross';
    default:
      return null;
  }
}

/** Shape for a panel's status kind; `null` when nothing is wrong or the panel is closed. */
export function panelStatusShape(kind: PanelStatus['kind'] | undefined): GlyphShape | null {
  switch (kind) {
    case 'exited':
      return 'square';
    case 'gone':
    case 'lost':
    case 'error':
    case 'streamFailed':
      return 'cross';
    default:
      return null;
  }
}

interface StateGlyphProps {
  shape: GlyphShape | null;
}

function Drawing({ shape }: { shape: GlyphShape }) {
  switch (shape) {
    case 'filled-dot':
      return <circle cx="6" cy="6" r="5" stroke="none" />;
    case 'half-ring':
      return (
        <>
          <circle cx="6" cy="6" r="4" fill="none" strokeWidth="2" />
          <path d="M6 2a4 4 0 0 1 0 8z" stroke="none" />
        </>
      );
    case 'ring':
      return <circle cx="6" cy="6" r="4" fill="none" strokeWidth="2" />;
    case 'square':
      return <rect x="2" y="2" width="8" height="8" rx="1" stroke="none" />;
    case 'dashed-ring':
      return <circle cx="6" cy="6" r="4" fill="none" strokeWidth="2" strokeDasharray="2 2" />;
    case 'cross':
      return (
        <path
          d="M2.5 2.5l7 7M9.5 2.5l-7 7"
          fill="none"
          strokeWidth="2"
          strokeLinecap="round"
        />
      );
    case 'check':
      return (
        <path
          d="M2 6.5l3 3 5-6"
          fill="none"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      );
  }
}

/** A decorative 12×12 shape beside a state word, so colour is never the only cue. */
export function StateGlyph({ shape }: StateGlyphProps) {
  if (shape === null) return null;
  return (
    <svg
      className="state-glyph"
      data-glyph={shape}
      width={12}
      height={12}
      viewBox="0 0 12 12"
      fill="currentColor"
      stroke="currentColor"
      aria-hidden="true"
      focusable="false"
    >
      <Drawing shape={shape} />
    </svg>
  );
}
