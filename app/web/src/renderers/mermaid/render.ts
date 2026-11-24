import { PeleError, render } from 'pele';
import type { Renderer } from '../../model/render';
import { failure, failureFrom, refuseOversizedSource, wrapFragment } from '../outcome';
import { sanitize } from '../sanitize/sanitize';
import './mermaid.css';

let count = 0;

export const renderMermaid: Renderer = async (source) => {
  try {
    const refused = refuseOversizedSource(source);
    if (refused) return refused;
    const result = await render(source, {
      limit: 204_800,
      maxEdges: 51_200,
      outputLimit: 4_000_000,
      linkSchemes: ['https', 'http'],
      imageSchemes: ['data'],
      idPrefix: `combine-mermaid-${++count}-`,
    });
    const content = sanitize(result.svg, 'svg');
    if (content.querySelector('svg') === null) {
      return failure('The diagram has no SVG after sanitizing.');
    }
    return { ok: true, fragment: wrapFragment('combine-mermaid', content) };
  } catch (e) {
    if (e instanceof PeleError) return failure(e.message, e.line, e.column);
    return failureFrom(e);
  }
};
