import type { MarkedText } from '../model/markedText';

export interface MarkedTextViewProps {
  text: MarkedText;
}

/** Renders visible text, with each code part in an inline `<code>`. */
export function MarkedTextView({ text }: MarkedTextViewProps) {
  if (typeof text === 'string') return <>{text}</>;
  return (
    <>
      {text.map((part, index) =>
        typeof part === 'string' ? part : <code key={index}>{part.code}</code>,
      )}
    </>
  );
}
