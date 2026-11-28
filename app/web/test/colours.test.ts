import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { filesUnder, read, relativeToWeb, srcRoot, stripComments, webRoot } from './support/css';

const TOKEN_FILES = [join(srcRoot, 'model/tokens.ts'), join(srcRoot, 'shell/tokens.css')];

const NAMED_COLOURS = `aliceblue antiquewhite aqua aquamarine azure beige bisque black blanchedalmond blue blueviolet brown
burlywood cadetblue chartreuse chocolate coral cornflowerblue cornsilk crimson cyan darkblue darkcyan darkgoldenrod
darkgray darkgreen darkgrey darkkhaki darkmagenta darkolivegreen darkorange darkorchid darkred darksalmon darkseagreen
darkslateblue darkslategray darkslategrey darkturquoise darkviolet deeppink deepskyblue dimgray dimgrey dodgerblue
firebrick floralwhite forestgreen fuchsia gainsboro ghostwhite gold goldenrod gray green greenyellow grey honeydew
hotpink indianred indigo ivory khaki lavender lavenderblush lawngreen lemonchiffon lightblue lightcoral lightcyan
lightgoldenrodyellow lightgray lightgreen lightgrey lightpink lightsalmon lightseagreen lightskyblue lightslategray
lightslategrey lightsteelblue lightyellow lime limegreen linen magenta maroon mediumaquamarine mediumblue mediumorchid
mediumpurple mediumseagreen mediumslateblue mediumspringgreen mediumturquoise mediumvioletred midnightblue mintcream
mistyrose moccasin navajowhite navy oldlace olive olivedrab orange orangered orchid palegoldenrod palegreen
paleturquoise palevioletred papayawhip peachpuff peru pink plum powderblue purple rebeccapurple red rosybrown royalblue
saddlebrown salmon sandybrown seagreen seashell sienna silver skyblue slateblue slategray slategrey snow springgreen
steelblue tan teal thistle tomato turquoise violet wheat white whitesmoke yellow yellowgreen`.split(/\s+/);

const SYSTEM_COLOURS = `accentcolor accentcolortext activetext buttonborder buttonface buttontext canvas canvastext
field fieldtext graytext highlight highlighttext linktext mark marktext selecteditem selecteditemtext visitedtext
activeborder activecaption appworkspace buttonhighlight buttonshadow captiontext inactiveborder inactivecaption
inactivecaptiontext infobackground infotext menu menutext scrollbar threeddarkshadow threedface threedhighlight
threedlightshadow threedshadow window windowframe windowtext`.split(/\s+/);

const COLOUR_WORDS = new Set([...NAMED_COLOURS, ...SYSTEM_COLOURS]);
const COLOUR_FUNCTION = /\b(?:rgba?|hsla?|hwb|lab|lch|oklab|oklch|color)\(/i;
const HEX = /#[0-9a-f]{3,8}\b/i;

const lineOf = (text: string, index: number): number => text.slice(0, index).split('\n').length;

function cssValueHits(css: string): number[] {
  const text = stripComments(css);
  const hits: number[] = [];
  for (const declaration of text.matchAll(/([a-z-]+|--[\w-]+)\s*:\s*([^;{}]*)/gi)) {
    const value = (declaration[2] ?? '')
      .replace(/"[^"]*"|'[^']*'/g, ' ')
      .replace(/url\([^)]*\)/gi, ' ')
      .replace(/--[\w-]+/g, ' ');
    const words = value.toLowerCase().match(/[a-z][a-z-]*/g) ?? [];
    const flagged =
      HEX.test(value) ||
      COLOUR_FUNCTION.test(value) ||
      words.some((word) => COLOUR_WORDS.has(word));
    if (flagged) hits.push(lineOf(text, declaration.index ?? 0));
  }
  return hits;
}

function tsLiteralHits(source: string): number[] {
  const hits: number[] = [];
  for (const literal of source.matchAll(/'(?:[^'\\\n]|\\.)*'|"(?:[^"\\\n]|\\.)*"|`(?:[^`\\]|\\.)*`/g)) {
    const text = literal[0];
    const hex = [...text.matchAll(new RegExp(HEX.source, 'gi'))].some(
      (found) => text[(found.index ?? 0) - 1] !== '&',
    );
    if (hex || COLOUR_FUNCTION.test(text)) hits.push(lineOf(source, literal.index ?? 0));
  }
  return hits;
}

describe('colours', () => {
  it('keeps both token files', () => {
    for (const path of TOKEN_FILES) expect(existsSync(path), path).toBe(true);
  });

  it('hard-codes no colour outside the token files', () => {
    const files = [
      ...filesUnder(srcRoot, ['.css', '.ts', '.tsx']),
      join(webRoot, 'index.html'),
    ].filter((path) => !TOKEN_FILES.includes(path));
    const hits = files.flatMap((path) => {
      const source = read(path);
      const lines = path.endsWith('.css')
        ? cssValueHits(source)
        : path.endsWith('.html')
          ? [...cssValueHits(source), ...tsLiteralHits(source)]
          : tsLiteralHits(source);
      return [...new Set(lines)].map((line) => `${relativeToWeb(path)}:${line}`);
    });
    expect(hits).toEqual([]);
  });
});
