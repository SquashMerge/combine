import MarkdownIt from 'markdown-it';
import type { Renderer } from '../../model/render';
import { failureFrom, refuseOversizedSource, wrapFragment } from '../outcome';
import { sanitize } from '../sanitize/sanitize';
import { highlightCode } from './highlight';
import './markdown.css';

const md = new MarkdownIt('default', {
  html: false,
  xhtmlOut: false,
  breaks: false,
  linkify: false,
  typographer: false,
  langPrefix: 'language-',
  highlight: (code, lang) => highlightCode(code, lang),
});

export const renderMarkdown: Renderer = async (source) => {
  try {
    const refused = refuseOversizedSource(source);
    if (refused) return refused;
    return {
      ok: true,
      fragment: wrapFragment('combine-markdown', sanitize(md.render(source), 'html')),
    };
  } catch (e) {
    return failureFrom(e);
  }
};
