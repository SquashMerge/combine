export interface RendererFixture {
  id: string;
  source: string;
  mustRender?: boolean;
  mustFail?: boolean;
}

const SVG_NS = 'xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"';

function svg(body: string): string {
  return `<svg ${SVG_NS}>${body}</svg>`;
}

const DATA_PNG = 'data:image/png;base64,iVBORw0KGgo=';
const DATA_GIF = 'data:image/gif;base64,R0lGODlhAQABAAAAACw=';
const DATA_SVG = 'data:image/svg+xml;base64,PHN2Zz4=';

/** Raw SVG through `sanitize(source, 'svg')`. */
export const SVG_FIXTURES: RendererFixture[] = [
  { id: 'S1', source: svg('<script>alert(1)</script><rect width="10" height="10"/>') },
  {
    id: 'S2',
    source: svg('<rect width="10" height="10" onload="alert(1)" onclick="alert(2)"/>'),
  },
  {
    id: 'S3',
    source: svg(
      '<foreignObject><div xmlns="http://www.w3.org/1999/xhtml"><img src="x" onerror="alert(1)"/></div></foreignObject>',
    ),
  },
  { id: 'S4', source: svg('<a href="javascript:alert(1)"><text>x</text></a>') },
  { id: 'S5', source: svg('<a xlink:href="javascript:alert(1)"><text>x</text></a>') },
  { id: 'S6', source: svg('<a href="https://example.com/ok"><text>x</text></a>') },
  { id: 'S7', source: svg('<a href="HTTPS://EXAMPLE.COM"><text>x</text></a>') },
  {
    id: 'S8',
    source: svg(
      [
        '//evil.example/x',
        '/relative',
        '#frag',
        'mailto:a@b.c',
        'data:text/html,x',
        ' javascript:alert(1)',
        'java&#9;script:alert(1)',
      ]
        .map((href) => `<a href="${href}"><text>x</text></a>`)
        .join(''),
    ),
  },
  {
    id: 'S9',
    source: svg(
      [
        '<image href="https://evil.example/a.png"/>',
        '<image xlink:href="//evil.example/b.png"/>',
        `<image href="${DATA_SVG}"/>`,
        `<image href="${DATA_PNG}"/>`,
      ].join(''),
    ),
  },
  {
    id: 'S10',
    source: svg(
      '<style>@import url(https://evil.example/x.css);</style>' +
        '<rect width="10" height="10" style="fill:url(https://evil.example/x)" fill="url(https://evil.example/y)"/>',
    ),
  },
  {
    id: 'S11',
    source: svg(
      '<defs><linearGradient id="grad"/><marker id="arrow"/></defs>' +
        '<rect width="10" height="10" style="fill:url(#grad);stroke:var(--pele-line,#000)" marker-end="url(#arrow)"/>',
    ),
    mustRender: true,
  },
  { id: 'S12', source: svg('<use href="https://evil.example/x.svg#x"/>') },
  {
    id: 'S13',
    source: svg(
      '<a href="#x"><animate attributeName="href" to="javascript:alert(1)"/><set attributeName="href" to="javascript:alert(1)"/></a>',
    ),
  },
  { id: 'S14', source: svg('<a data-combine-open-url="javascript:alert(1)"><text>x</text></a>') },
  {
    id: 'S15',
    source: svg('<rect width="10" height="10" style="background:\\75rl(https://evil.example/x)"/>'),
  },
  {
    id: 'S16',
    source: svg(
      '<iframe src="https://evil.example"></iframe><object data="https://evil.example"></object><embed src="https://evil.example"/>',
    ),
  },
  { id: 'S17', source: svg(`<image href="${DATA_PNG}" onerror="alert(1)"/>`) },
  {
    id: 'S18',
    source:
      '<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"><filter id="f"><feImage href="https://evil.example/a.png" xlink:href="https://evil.example/b.png"/></filter><rect width="10" height="10"/></svg>',
  },
];

/** Raw HTML through `sanitize(source, 'html')`. */
export const HTML_FIXTURES: RendererFixture[] = [
  { id: 'H1', source: '<script>alert(1)</script><p onclick="alert(2)">text</p>' },
  {
    id: 'H2',
    source: [
      '<img src="https://evil.example/a.png">',
      '<img src="//evil.example/b.png">',
      `<img src="${DATA_SVG}">`,
      `<img src="${DATA_PNG}" srcset="https://evil.example/c.png 2x">`,
      `<img src="${DATA_GIF}">`,
    ].join(''),
  },
  {
    id: 'H3',
    source:
      '<a href="https://example.com/a">a</a><a href="javascript:alert(1)">b</a><a href="./rel">c</a>',
  },
  {
    id: 'H4',
    source:
      '<iframe src="https://evil.example"></iframe><style>p{color:red}</style>' +
      '<form action="https://evil.example"><p>f</p></form>' +
      '<svg><script>alert(1)</script></svg><math><mi>x</mi></math>',
  },
  { id: 'H5', source: '<p style="background:url(https://evil.example/x)">text</p>' },
  { id: 'H6', source: '<a data-combine-open-url="javascript:alert(1)">x</a>' },
];

function clickFlowchart(click: string): string {
  return `flowchart LR\n  A --> B\n  ${click}`;
}

/** Mermaid sources through `renderMermaid`. */
export const MERMAID_FIXTURES: RendererFixture[] = [
  { id: 'M1', source: 'flowchart LR\n  A["<script>alert(1)</script>"] --> B', mustRender: true },
  { id: 'M2', source: 'flowchart LR\n  A["<img src=x onerror=alert(1)>"] --> B', mustRender: true },
  { id: 'M3', source: clickFlowchart('click A href "javascript:alert(1)"') },
  {
    id: 'M4',
    source:
      'flowchart LR\n  A --> B\n  C --> D\n  E --> F\n  G --> H\n' +
      '  click A href "data:text/html,<script>alert(1)</script>"\n' +
      '  click C href "vbscript:x"\n' +
      '  click E href "file:///etc/passwd"\n' +
      '  click G href "mailto:a@b.c"',
  },
  { id: 'M5', source: clickFlowchart('click A href "//evil.example/x"') },
  {
    id: 'M-link',
    source: clickFlowchart('click A href "https://example.com/a"'),
    mustRender: true,
  },
  {
    id: 'M6',
    source:
      'flowchart LR\n  A --> B\n  classDef bad fill:url(https://evil.example/x.svg),stroke:#f00\n  class A bad',
    mustFail: true,
  },
  {
    id: 'M7',
    source:
      'flowchart LR\n  A --> B\n  style A fill:#f00,background-image:url(https://evil.example/p.png)',
    mustFail: true,
  },
  {
    id: 'M8',
    source:
      '%%{init: {"themeCSS": "* { background: url(https://evil.example/x) }", "securityLevel": "loose"}}%%\nflowchart LR\n  A --> B',
  },
  { id: 'M9', source: clickFlowchart('click A call alert(1)') },
  { id: 'M10', source: 'flowchart LR\n  A["$$x^2$$"] --> B' },
  {
    id: 'M11',
    source: 'flowchart LR\n  A --> B\n  style A color:red;background:url(https://evil.example/x)',
    mustRender: true,
  },
];

const MD3_LINKS = [
  '[a](javascript:alert(1))',
  '[b](JAVASCRIPT:alert(1))',
  '[c](vbscript:x)',
  '[d](file:///etc/passwd)',
  '[e](data:text/html,x)',
  '[f](mailto:a@b.c)',
  '[g](ftp://example.com)',
  '[h](//evil.example)',
  '[i](/rel)',
  '[j](#frag)',
  '[k](https://example.com/k)',
  '[l](HTTP://EXAMPLE.COM/l)',
  '<https://example.com/auto>',
  '[m][ref]',
];

const MD4_IMAGES = [
  '![r](https://evil.example/r.png)',
  '![p](//evil.example/p.png)',
  '![rel](./x.png)',
  '![svg](data:image/svg+xml;base64,PHN2Zz4=)',
  '![png](data:image/png;base64,iVBORw0KGgo=)',
  '![jpeg](data:image/jpeg;base64,/9j/)',
  '![gif](data:image/gif;base64,R0lGODlh)',
  '![webp](data:image/webp;base64,UklGRg==)',
  '![ref][img]',
];

/** Markdown sources through `renderMarkdown`. */
export const MARKDOWN_FIXTURES: RendererFixture[] = [
  {
    id: 'MD1',
    source: '<script>alert(1)</script>\n\ntext <script>alert(1)</script> text',
    mustRender: true,
  },
  {
    id: 'MD2',
    source: '<img src=x onerror=alert(1)>\n\n<div onclick="x">y</div>',
    mustRender: true,
  },
  {
    id: 'MD3',
    source: `${MD3_LINKS.join('\n\n')}\n\n[ref]: https://example.com/m`,
    mustRender: true,
  },
  {
    id: 'MD4',
    source: `${MD4_IMAGES.join('\n\n')}\n\n[img]: https://evil.example/i.png`,
    mustRender: true,
  },
  { id: 'MD6', source: '```"><img src=x onerror=alert(1)>\ncode\n```', mustRender: true },
  { id: 'MD9', source: '[x](&#106;avascript:alert(1))', mustRender: true },
];
