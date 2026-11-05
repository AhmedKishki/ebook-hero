'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { test } = require('node:test');
const markdown = require('./markdown');

const BASE = 'https://github.com/AhmedKishki/ebook-hero/blob/main/docs/';
const html = (text, options) => markdown.render(text, options).html;

test('headings get GitHub ids: lower case, punctuation dropped, spaces as dashes, repeats numbered', () => {
  const { html: page, headings } = markdown.render([
    '# Using ebook-hero',
    '## Where files are kept?',
    '### outline',
    '### outline',
    '### book_info',
    '## The `read` command ##',
    '###### six',
    '####### seven',
  ].join('\n'));
  assert.deepEqual(headings, [
    { level: 1, id: 'using-ebook-hero', text: 'Using ebook-hero' },
    { level: 2, id: 'where-files-are-kept', text: 'Where files are kept?' },
    { level: 3, id: 'outline', text: 'outline' },
    { level: 3, id: 'outline-1', text: 'outline' },
    { level: 3, id: 'book_info', text: 'book_info' },
    { level: 2, id: 'the-read-command', text: 'The read command' },
    { level: 6, id: 'six', text: 'six' },
  ]);
  assert.match(page, /<h2 id="the-read-command">The <code>read<\/code> command<\/h2>/);
  assert.match(page, /<p>####### seven<\/p>/);
});

test('ids are the same on every render, and a prefix goes before every id and # link', () => {
  const text = '## Cache\n\nSee [the cache](#cache).';
  assert.equal(html(text), html(text));
  assert.equal(
    html(text, { idPrefix: 'help-' }),
    '<h2 id="help-cache">Cache</h2>\n<p>See <a href="#help-cache">the cache</a>.</p>',
  );
});

test('lines without a blank between them are one paragraph', () => {
  assert.equal(html('one\ntwo\n\nthree'), '<p>one\ntwo</p>\n<p>three</p>');
});

test('a tight list holds its items\' text, a loose one gives each a paragraph', () => {
  assert.equal(html('- a\n- b'), '<ul><li>a</li><li>b</li></ul>');
  assert.equal(html('* a\n\n* b'), '<ul><li><p>a</p></li><li><p>b</p></li></ul>');
});

test('an ordered list keeps its first number', () => {
  assert.equal(html('1. one\n2. two'), '<ol><li>one</li><li>two</li></ol>');
  assert.equal(html('3) three\n4) four'), '<ol start="3"><li>three</li><li>four</li></ol>');
});

test('an indented list nests in the item above it, and a line without indent continues the item', () => {
  assert.equal(
    html('- fruit\n  - apple\n  - pear\n- veg\ncarrots too\n\nafter'),
    '<ul><li>fruit\n<ul><li>apple</li><li>pear</li></ul></li><li>veg\ncarrots too</li></ul>\n<p>after</p>',
  );
  assert.equal(html('1. first\n   1. inner\n2. second'), '<ol><li>first\n<ol><li>inner</li></ol></li><li>second</li></ol>');
});

test('a numbered line inside a paragraph does not start a list unless it is 1', () => {
  assert.equal(html('in 2020\n2. not a list'), '<p>in 2020\n2. not a list</p>');
  assert.equal(html('steps:\n1. first'), '<p>steps:</p>\n<ol><li>first</li></ol>');
});

test('a fenced block with any language tag is code, escaped and not read as Markdown', () => {
  for (const tag of ['', 'sh', 'console', 'json', 'rust', 'text', 'made-up']) {
    assert.equal(
      html(`\`\`\`${tag}\n# not a heading\n| a | b |\n**x** <b>&\n\`\`\``),
      '<pre><code># not a heading\n| a | b |\n**x** &lt;b&gt;&amp;</code></pre>',
    );
  }
  assert.equal(html('~~~~\n```\nstill code\n~~~~\nafter'), '<pre><code>```\nstill code</code></pre>\n<p>after</p>');
  assert.equal(html('```\nnever closed\n\n## still code'), '<pre><code>never closed\n\n## still code</code></pre>');
  assert.equal(html('  ```\n  indented\n    more\n  ```'), '<pre><code>indented\n  more</code></pre>');
});

test('inline code is escaped, and a longer backtick run can hold a backtick', () => {
  assert.equal(html('run `ebook-hero <path>` now'), '<p>run <code>ebook-hero &lt;path&gt;</code> now</p>');
  assert.equal(html('``a ` b``'), '<p><code>a ` b</code></p>');
  assert.equal(html('`` `x` ``'), '<p><code>`x`</code></p>');
  assert.equal(html('a `*not emphasis*` b'), '<p>a <code>*not emphasis*</code> b</p>');
  assert.equal(html('one ` alone'), '<p>one ` alone</p>');
});

test('bold and italic, nested or alone, with * or _', () => {
  assert.equal(html('**bold** and *italic*'), '<p><strong>bold</strong> and <em>italic</em></p>');
  assert.equal(html('__bold__ and _italic_'), '<p><strong>bold</strong> and <em>italic</em></p>');
  assert.equal(html('*a **b** c*'), '<p><em>a <strong>b</strong> c</em></p>');
  assert.equal(html('***both***'), '<p><strong><em>both</em></strong></p>');
  assert.equal(html('*the `x` here*'), '<p><em>the <code>x</code> here</em></p>');
});

test('_ inside a word, * between spaces and unclosed markers stay as written', () => {
  assert.equal(html('book_info and verify_quote'), '<p>book_info and verify_quote</p>');
  assert.equal(html('2 * 3 * 4'), '<p>2 * 3 * 4</p>');
  assert.equal(html('**not closed'), '<p>**not closed</p>');
  assert.equal(html('\\*not italic\\*'), '<p>*not italic*</p>');
});

test('a # link stays in the guide and an http(s) link is marked to open in the browser', () => {
  assert.equal(html('[info](#info)'), '<p><a href="#info">info</a></p>');
  assert.equal(
    html('[docs.rs](https://docs.rs/ebook-hero)'),
    '<p><a href="https://docs.rs/ebook-hero" data-external title="https://docs.rs/ebook-hero">docs.rs</a></p>',
  );
  assert.match(html('[x](http://example.com/a_(b))'), /href="http:\/\/example.com\/a_\(b\)" data-external/);
  assert.match(html('[x](<https://example.com/a b> "a title")'), /href="https:\/\/example.com\/a%20b" data-external/);
  assert.equal(html('[`cache list`](#cache)'), '<p><a href="#cache"><code>cache list</code></a></p>');
});

test('a link to another file of docs/ opens it on GitHub, or is plain text without a base', () => {
  assert.equal(
    html('[search.md](search.md)', { base: BASE }),
    `<p><a href="${BASE}search.md" data-external title="${BASE}search.md">search.md</a></p>`,
  );
  assert.match(html('[readme](../README.md#install)', { base: BASE }), /href="https:\/\/github.com\/AhmedKishki\/ebook-hero\/blob\/main\/README.md#install"/);
  assert.equal(html('[search.md](search.md)'), '<p>search.md</p>');
});

test('a link to any other scheme or site, or to nowhere, is shown as its text', () => {
  for (const dest of ['javascript:alert(1)', 'JaVaScRiPt:alert(1)', 'data:text/html,x', 'file:///etc/passwd', 'mailto:a@b.c', '//evil.example/x', '']) {
    assert.equal(html(`[label](${dest})`, { base: BASE }), '<p>label</p>', dest);
  }
});

test('a table keeps its header, alignment and every row, padding short rows', () => {
  const page = html([
    '| Option | Default | Meaning |',
    '|:---|:---:|---:|',
    '| `--mode <mode>` | `hybrid` | How to *rank* |',
    '| `a \\| b` | x |',
    '',
    'after',
  ].join('\n'));
  assert.equal(page, '<div class="table"><table>'
    + '<thead><tr><th style="text-align: left">Option</th><th style="text-align: center">Default</th><th style="text-align: right">Meaning</th></tr></thead>'
    + '<tbody>'
    + '<tr><td style="text-align: left"><code>--mode &lt;mode&gt;</code></td><td style="text-align: center"><code>hybrid</code></td><td style="text-align: right">How to <em>rank</em></td></tr>'
    + '<tr><td style="text-align: left"><code>a | b</code></td><td style="text-align: center">x</td><td style="text-align: right"></td></tr>'
    + '</tbody></table></div>\n<p>after</p>');
});

test('a row of pipes without a delimiter row under it is a paragraph', () => {
  assert.equal(html('| a | b |\n| c | d |'), '<p>| a | b |\n| c | d |</p>');
  assert.equal(html('| a | b |\n|---|'), '<p>| a | b |\n|---|</p>');
});

test('a block quote holds blocks, and a line without > carries on its paragraph', () => {
  assert.equal(html('> one\ntwo\n>\n> - item'), '<blockquote><p>one\ntwo</p>\n<ul><li>item</li></ul></blockquote>');
});

test('text that could be markup is escaped wherever it appears', () => {
  const attack = '<img src=x onerror="alert(1)"> & \'q\'';
  const escaped = '&lt;img src=x onerror=&quot;alert(1)&quot;&gt; &amp; &#39;q&#39;';
  assert.equal(html(attack), `<p>${escaped}</p>`);
  assert.equal(html(`## ${attack}`), `<h2 id="img-srcx-onerroralert1--q">${escaped}</h2>`);
  assert.equal(html(`- ${attack}`), `<ul><li>${escaped}</li></ul>`);
  assert.equal(html(`> ${attack}`), `<blockquote><p>${escaped}</p></blockquote>`);
  assert.ok(html(`| h |\n|---|\n| ${attack} |`).includes(`<td>${escaped}</td>`));
  assert.equal(html(`[${attack}](#x)`), `<p><a href="#x">${escaped}</a></p>`);
  assert.equal(html('<script>alert(1)</script>'), '<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>');
});

test('a link address cannot break out of its attribute', () => {
  const page = html('[x](https://a.example/"onmouseover="alert(1)) [y](#a"b)');
  assert.ok(!page.includes('"onmouseover'), page);
  assert.ok(!page.includes('#a"b'), page);
  assert.match(page, /href="https:\/\/a.example\/%22onmouseover=%22alert\(1\)"/);
  assert.match(page, /href="#a&quot;b"/);
});

test('Markdown it does not know is shown as escaped text', () => {
  assert.equal(html('<div class="x">raw</div>'), '<p>&lt;div class=&quot;x&quot;&gt;raw&lt;/div&gt;</p>');
  assert.equal(html('<https://example.com>'), '<p>&lt;https://example.com&gt;</p>');
  assert.equal(html('[ref][1]\n\n[1]: https://example.com'), '<p>[ref][1]</p>\n<p>[1]: https://example.com</p>');
  assert.equal(html('~~struck~~'), '<p>~~struck~~</p>');
});

test('the contents nest each heading under the one above it, from level 2 to 3', () => {
  const { headings } = markdown.render([
    '# Title', '## Commands', '### info', '#### detail', '### search', '## MCP reference', '### search', '## Recipes',
  ].join('\n\n'));
  const entry = (id, text, level, children = []) => ({ id, text, level, children });
  assert.deepEqual(markdown.contents(headings), [
    entry('commands', 'Commands', 2, [entry('info', 'info', 3), entry('search', 'search', 3)]),
    entry('mcp-reference', 'MCP reference', 2, [entry('search-1', 'search', 3)]),
    entry('recipes', 'Recipes', 2),
  ]);
});

test('the contents put a heading that skips a level under the nearest one above it', () => {
  const { headings } = markdown.render('## A\n\n#### deep\n\n### B');
  assert.deepEqual(markdown.contents(headings, { from: 2, to: 4 }).map((e) => [e.id, e.children.map((c) => c.id)]), [['a', ['deep', 'b']]]);
  assert.deepEqual(markdown.contents(markdown.render('### alone').headings), [{ id: 'alone', text: 'alone', level: 3, children: [] }]);
});

// The real guide, as the Help tab shows it.
const GUIDE = path.join(__dirname, '..', 'docs', 'usage.md');

/** The lines of `text` outside fenced code. */
function outsideFences(text) {
  let fenced = false;
  return text.split('\n').filter((line) => {
    if (/^```/.test(line)) {
      fenced = !fenced;
      return false;
    }
    return !fenced;
  });
}

test('docs/usage.md: every heading has an id, and every contents entry and # link has its target', () => {
  const source = fs.readFileSync(GUIDE, 'utf8');
  const { html: page, headings } = markdown.render(source, { base: BASE, idPrefix: 'help-' });
  const ids = [...page.matchAll(/<h[1-6] id="([^"]+)">/g)].map((m) => m[1]);
  const sourceHeadings = outsideFences(source).filter((line) => /^#{1,6} /.test(line));
  assert.equal(ids.length, sourceHeadings.length);
  assert.equal(new Set(ids).size, ids.length, 'heading ids repeat');
  assert.deepEqual(headings.map((h) => h.id), ids);

  const flat = (entries) => entries.flatMap((e) => [e, ...flat(e.children)]);
  const entries = flat(markdown.contents(headings));
  assert.equal(entries.length, headings.filter((h) => h.level === 2 || h.level === 3).length);
  for (const entry of entries) assert.ok(ids.includes(entry.id), `no heading for contents entry ${entry.id}`);

  const anchors = [...page.matchAll(/<a href="#([^"]+)">/g)].map((m) => m[1]);
  assert.equal(anchors.length, (source.match(/\]\(#/g) ?? []).length);
  for (const anchor of anchors) assert.ok(ids.includes(anchor), `the link #${anchor} has no heading`);
});

test('docs/usage.md: no Markdown marker shows outside code, and every table has its rows', () => {
  const source = fs.readFileSync(GUIDE, 'utf8');
  const page = html(source, { base: BASE, idPrefix: 'help-' });
  const prose = page.replace(/<pre>[\s\S]*?<\/pre>/g, '').replace(/<code>[\s\S]*?<\/code>/g, '');
  for (const marker of ['**', '```', '`', '|---', '](', '<p>#', '<p>|', '<p>- ', '<p>&gt; ']) {
    assert.ok(!prose.includes(marker), `${marker} shows in the guide's text`);
  }

  // Each table in the source: its delimiter row, then a row per line until a blank one.
  const lines = outsideFences(source);
  const expected = [];
  lines.forEach((line, i) => {
    if (!/^\|[-:| ]+\|$/.test(line)) return;
    let rows = 0;
    while (lines[i + 1 + rows]?.startsWith('|')) rows++;
    expected.push(rows);
  });
  const tables = [...page.matchAll(/<table>([\s\S]*?)<\/table>/g)].map((m) => (m[1].match(/<tr>/g) ?? []).length - 1);
  assert.ok(expected.length > 0, 'found no table in the guide');
  assert.deepEqual(tables, expected);

  assert.equal((page.match(/<pre><code>/g) ?? []).length, (source.match(/^```/gm) ?? []).length / 2);
});
