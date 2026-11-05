'use strict';
// The Markdown of the usage guide as HTML, for the Help pane, and the contents
// list built from its headings. It handles headings, paragraphs, lists, block
// quotes, fenced code, tables, inline code, emphasis and links, which is what
// docs/usage.md uses, and shows any other Markdown as plain text. Every piece
// of text is escaped before it goes into the HTML, and a link leads only to a
// heading of the guide or to an http(s) address, so the file cannot add markup
// or script to the window. It has no DOM in it, so the window loads it as a
// script and `node --test` requires it.

const markdown = (() => {
  const ENTITIES = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
  const escape = (text) => String(text).replace(/[&<>"']/g, (ch) => ENTITIES[ch]);

  // ASCII punctuation, which a backslash before it makes literal.
  const PUNCTUATION = /^[!-/:-@[-`{-~]$/;
  const WORD = /[\p{L}\p{N}]/u;
  const SPACE = /\s/;

  const FENCE = /^( {0,3})(`{3,}|~{3,})(.*)$/;
  const FENCE_CLOSE = /^ {0,3}(`{3,}|~{3,})[ \t]*$/;
  const HEADING = /^ {0,3}(#{1,6})(?:[ \t]+(.*?))?[ \t]*$/;
  const RULE = /^ {0,3}(?:(?:-[ \t]*){3,}|(?:\*[ \t]*){3,}|(?:_[ \t]*){3,})$/;
  const QUOTE = /^ {0,3}> ?/;
  const ITEM = /^( {0,3})([-+*]|(\d{1,9})([.)]))(?:([ \t]+)(.*))?$/;
  const DELIMITER_CELL = /^:?-+:?$/;

  const isBlank = (line) => /^[ \t]*$/.test(line);
  const indentOf = (line) => /^ */.exec(line)[0].length;
  const run = (text, i) => {
    let j = i;
    while (text[j] === text[i]) j++;
    return j - i;
  };
  const isEscape = (text, i) => text[i] === '\\' && PUNCTUATION.test(text[i + 1] ?? '');

  /** Leading tabs as spaces, to the next multiple of four, so indentation can be counted in spaces. */
  function expandTabs(line) {
    return line.replace(/^[ \t]+/, (lead) => {
      let out = '';
      for (const ch of lead) out += ch === '\t' ? ' '.repeat(4 - (out.length % 4)) : ch;
      return out;
    });
  }

  function fenceAt(line) {
    const m = FENCE.exec(line);
    // A backtick fence's info string cannot hold a backtick, or ```a``` would open a block.
    if (!m || (m[2][0] === '`' && m[3].includes('`'))) return null;
    return { indent: m[1].length, marker: m[2] };
  }

  function closesFence(line, fence) {
    const m = FENCE_CLOSE.exec(line);
    return m !== null && m[1][0] === fence.marker[0] && m[1].length >= fence.marker.length;
  }

  /** The list item that `line` starts, or null. `offset` is the column its content starts at. */
  function itemAt(line) {
    const m = ITEM.exec(line);
    if (!m) return null;
    const [, indent, marker, number, delimiter, spaces = '', text = ''] = m;
    const gap = text && spaces.length <= 4 ? spaces.length : 1;
    return {
      ordered: number !== undefined,
      kind: delimiter ?? marker,
      start: number === undefined ? null : Number(number),
      offset: indent.length + marker.length + gap,
      text: text && spaces.length > 4 ? spaces.slice(1) + text : text,
    };
  }

  /** Whether `line` ends a paragraph by starting a block of its own. */
  function interrupts(line) {
    if (fenceAt(line) || HEADING.test(line) || RULE.test(line) || QUOTE.test(line)) return true;
    // As in CommonMark, an empty item, or a numbered one that does not start at 1, cannot interrupt a paragraph.
    const item = itemAt(line);
    return item !== null && item.text.trim() !== '' && (!item.ordered || item.start === 1);
  }

  /** A table row's cells. A pipe splits cells unless a backslash escapes it, even inside a code span, as in GitHub's tables. */
  function splitRow(line) {
    let row = line.trim();
    if (row.startsWith('|')) row = row.slice(1);
    if (row.endsWith('|') && !row.endsWith('\\|')) row = row.slice(0, -1);
    const cells = [];
    let cell = '';
    for (let i = 0; i < row.length; i++) {
      if (row[i] === '\\' && row[i + 1] === '|') {
        cell += '|';
        i++;
      } else if (row[i] === '|') {
        cells.push(cell.trim());
        cell = '';
      } else {
        cell += row[i];
      }
    }
    cells.push(cell.trim());
    return cells;
  }

  const hasPipe = (line) => /(^|[^\\])\|/.test(line);

  /** The head of the table that starts at line `i`: a row, then a delimiter row with as many cells. Null for no table. */
  function tableAt(lines, i) {
    if (i + 1 >= lines.length || !hasPipe(lines[i]) || !hasPipe(lines[i + 1])) return null;
    const head = splitRow(lines[i]);
    const delimiter = splitRow(lines[i + 1]);
    if (delimiter.length !== head.length || !delimiter.every((cell) => DELIMITER_CELL.test(cell))) return null;
    const align = delimiter.map((cell) => {
      if (cell.startsWith(':') && cell.endsWith(':')) return 'center';
      if (cell.endsWith(':')) return 'right';
      return cell.startsWith(':') ? 'left' : null;
    });
    return { head, align };
  }

  /** The blocks of `lines`, each `{ type, ... }`, in order. */
  function parseBlocks(lines) {
    const blocks = [];
    let i = 0;
    while (i < lines.length) {
      const line = lines[i];
      if (isBlank(line)) {
        i++;
        continue;
      }
      const fence = fenceAt(line);
      if (fence) {
        const body = [];
        for (i++; i < lines.length && !closesFence(lines[i], fence); i++) {
          body.push(lines[i].replace(new RegExp(`^ {0,${fence.indent}}`), ''));
        }
        // Past the closing fence; a fence that is never closed runs to the end, as in CommonMark.
        i++;
        blocks.push({ type: 'code', text: body.join('\n') });
        continue;
      }
      const heading = HEADING.exec(line);
      if (heading) {
        const text = (heading[2] ?? '').replace(/(?:^|[ \t]+)#+$/, '').trim();
        blocks.push({ type: 'heading', level: heading[1].length, text });
        i++;
        continue;
      }
      if (RULE.test(line)) {
        blocks.push({ type: 'rule' });
        i++;
        continue;
      }
      if (QUOTE.test(line)) {
        const inner = [];
        for (; i < lines.length; i++) {
          const next = lines[i];
          if (QUOTE.test(next)) inner.push(next.replace(QUOTE, ''));
          // A line without `>` continues the quote's paragraph, as in CommonMark.
          else if (!isBlank(next) && inner.length > 0 && !isBlank(inner[inner.length - 1]) && !interrupts(next)) inner.push(next);
          else break;
        }
        blocks.push({ type: 'quote', children: parseBlocks(inner) });
        continue;
      }
      if (itemAt(line)) {
        const list = parseList(lines, i);
        blocks.push(list.block);
        i = list.end;
        continue;
      }
      const table = tableAt(lines, i);
      if (table) {
        const rows = [];
        for (i += 2; i < lines.length && !isBlank(lines[i]) && !interrupts(lines[i]); i++) {
          const cells = splitRow(lines[i]);
          rows.push(table.head.map((_, c) => cells[c] ?? ''));
        }
        blocks.push({ type: 'table', ...table, rows });
        continue;
      }
      const text = [line.trim()];
      for (i++; i < lines.length && !isBlank(lines[i]) && !interrupts(lines[i]) && !tableAt(lines, i); i++) {
        text.push(lines[i].trim());
      }
      blocks.push({ type: 'paragraph', text: text.join('\n') });
    }
    return blocks;
  }

  /**
   * The list that starts at line `start`: `{ block, end }`. An item holds the
   * lines indented to its content, which may be a nested list, and a line that
   * carries on its last paragraph. The list is loose, and its items'
   * paragraphs get `<p>`, when a blank line separates two items or two blocks
   * of one item.
   */
  function parseList(lines, start) {
    const first = itemAt(lines[start]);
    const items = [];
    let loose = false;
    let i = start;
    while (i < lines.length) {
      const item = RULE.test(lines[i]) ? null : itemAt(lines[i]);
      if (!item || item.ordered !== first.ordered || item.kind !== first.kind) break;
      const body = [item.text];
      let j = i + 1;
      for (; j < lines.length; j++) {
        const next = lines[j];
        if (isBlank(next)) body.push('');
        else if (indentOf(next) >= item.offset) body.push(next.slice(item.offset));
        else if (!isBlank(body[body.length - 1]) && !interrupts(next) && !itemAt(next)) body.push(next.trim());
        else break;
      }
      let blanks = 0;
      while (body.length > 1 && isBlank(body[body.length - 1])) {
        body.pop();
        blanks++;
      }
      if (body.some(isBlank)) loose = true;
      items.push(parseBlocks(body));
      i = j;
      const sibling = i < lines.length && !RULE.test(lines[i]) ? itemAt(lines[i]) : null;
      if (blanks > 0 && sibling && sibling.ordered === first.ordered && sibling.kind === first.kind) loose = true;
    }
    return { block: { type: 'list', ordered: first.ordered, start: first.start, loose, items }, end: i };
  }

  /** The code span that starts at `i`: `{ value, end }`, or null when no run of as many backticks closes it. */
  function codeSpan(text, i) {
    const n = run(text, i);
    for (let j = text.indexOf('`', i + n); j !== -1; j = text.indexOf('`', j)) {
      const m = run(text, j);
      if (m === n) {
        let value = text.slice(i + n, j).replace(/\n/g, ' ');
        if (value.length >= 2 && value[0] === ' ' && value[value.length - 1] === ' ' && value.trim() !== '') {
          value = value.slice(1, -1);
        }
        return { value, end: j + m };
      }
      j += m;
    }
    return null;
  }

  /** The inline link that starts at `i`, `[label](destination "title")`: `{ label, dest, end }`, or null. */
  function linkAt(text, i) {
    let depth = 0;
    let j = i;
    for (; j < text.length; j++) {
      if (isEscape(text, j)) {
        j++;
      } else if (text[j] === '`') {
        const span = codeSpan(text, j);
        j = (span ? span.end : j + run(text, j)) - 1;
      } else if (text[j] === '[') {
        depth++;
      } else if (text[j] === ']' && --depth === 0) {
        break;
      }
    }
    if (j >= text.length || text[j + 1] !== '(') return null;
    let k = j + 2;
    while (SPACE.test(text[k] ?? '')) k++;
    let dest;
    if (text[k] === '<') {
      const close = text.indexOf('>', k);
      if (close < 0 || text.slice(k, close).includes('\n')) return null;
      dest = text.slice(k + 1, close);
      k = close + 1;
    } else {
      const from = k;
      let parens = 0;
      for (; k < text.length; k++) {
        if (isEscape(text, k)) k++;
        else if (SPACE.test(text[k])) break;
        else if (text[k] === '(') parens++;
        else if (text[k] === ')' && parens-- === 0) break;
      }
      dest = text.slice(from, k);
    }
    while (SPACE.test(text[k] ?? '')) k++;
    const closer = { '"': '"', "'": "'", '(': ')' }[text[k]];
    if (closer) {
      const close = text.indexOf(closer, k + 1);
      if (close < 0) return null;
      k = close + 1;
      while (SPACE.test(text[k] ?? '')) k++;
    }
    if (text[k] !== ')') return null;
    return { label: text.slice(i + 1, j), dest: dest.replace(/\\([!-/:-@[-`{-~])/g, '$1'), end: k + 1 };
  }

  /**
   * The emphasis that starts at `i`: `{ strong, inner, end }`, or null. `**`
   * is strong and `*` emphasis, and so are `__` and `_`, which cannot open or
   * close inside a word, so `book_info` stays as it is. An opener is followed by
   * a character other than a space, and a closer follows one.
   */
  function emphasisAt(text, i) {
    const ch = text[i];
    const n = run(text, i) >= 2 ? 2 : 1;
    const after = text[i + n];
    if (after === undefined || SPACE.test(after)) return null;
    if (ch === '_' && i > 0 && WORD.test(text[i - 1])) return null;
    let j = i + n;
    while (j < text.length) {
      if (isEscape(text, j)) {
        j += 2;
        continue;
      }
      if (text[j] === '`') {
        const span = codeSpan(text, j);
        j = span ? span.end : j + run(text, j);
        continue;
      }
      if (text[j] === '[') {
        const link = linkAt(text, j);
        if (link) {
          j = link.end;
          continue;
        }
      }
      if (text[j] === ch) {
        const m = run(text, j);
        // The closer is the last n characters of its run, so ***x*** is strong around emphasis.
        const close = j + m - n;
        const fits = n === 1 ? m === 1 : m >= 2;
        if (fits && close > i + n && !SPACE.test(text[j - 1]) && (ch !== '_' || !WORD.test(text[j + m] ?? ''))) {
          return { strong: n === 2, inner: text.slice(i + n, close), end: j + m };
        }
        j += m;
        continue;
      }
      j++;
    }
    return null;
  }

  /** The inline nodes of `text`: text, code, em, strong and link. A link's label holds no link. */
  function parseInline(text, inLink = false) {
    const nodes = [];
    let plainText = '';
    const push = (node) => {
      if (plainText) nodes.push({ type: 'text', value: plainText });
      plainText = '';
      nodes.push(node);
    };
    let i = 0;
    while (i < text.length) {
      const ch = text[i];
      if (isEscape(text, i)) {
        plainText += text[i + 1];
        i += 2;
        continue;
      }
      if (ch === '`') {
        const span = codeSpan(text, i);
        if (span) {
          push({ type: 'code', value: span.value });
          i = span.end;
        } else {
          plainText += text.slice(i, i + run(text, i));
          i += run(text, i);
        }
        continue;
      }
      if (ch === '[' && !inLink) {
        const link = linkAt(text, i);
        if (link) {
          push({ type: 'link', dest: link.dest, children: parseInline(link.label, true) });
          i = link.end;
          continue;
        }
      }
      if (ch === '*' || ch === '_') {
        const span = emphasisAt(text, i);
        if (span) {
          push({ type: span.strong ? 'strong' : 'em', children: parseInline(span.inner, inLink) });
          i = span.end;
        } else {
          plainText += text.slice(i, i + run(text, i));
          i += run(text, i);
        }
        continue;
      }
      plainText += ch;
      i++;
    }
    if (plainText) nodes.push({ type: 'text', value: plainText });
    return nodes;
  }

  /** The text of inline nodes without their markup, as a heading's id and contents entry use it. */
  function plain(nodes) {
    return nodes.map((node) => (node.children ? plain(node.children) : node.value)).join('');
  }

  /**
   * Where a link leads: `{ href, external }`, or null to show its label as
   * plain text. `#heading` stays in the guide. An http(s) address is external,
   * and so is a relative one, such as `search.md`, resolved against `base` when
   * it stays on the same site. Any other scheme, such as `javascript:` or
   * `file:`, and a relative link without a base, lead nowhere.
   */
  function target(dest, { base, idPrefix }) {
    if (dest.startsWith('#')) {
      let id = dest.slice(1);
      try {
        id = decodeURIComponent(id);
      } catch {
        // A malformed escape is kept as written.
      }
      return { href: `#${idPrefix}${id}`, external: false };
    }
    try {
      if (/^https?:\/\//i.test(dest)) return { href: new URL(dest).href, external: true };
      if (!dest || /^[a-z][a-z0-9+.-]*:/i.test(dest) || !base) return null;
      const url = new URL(dest, base);
      return url.origin === new URL(base).origin ? { href: url.href, external: true } : null;
    } catch {
      return null;
    }
  }

  function inlineHtml(nodes, context) {
    return nodes.map((node) => {
      switch (node.type) {
        case 'code':
          return `<code>${escape(node.value)}</code>`;
        case 'em':
          return `<em>${inlineHtml(node.children, context)}</em>`;
        case 'strong':
          return `<strong>${inlineHtml(node.children, context)}</strong>`;
        case 'link': {
          const label = inlineHtml(node.children, context);
          const to = target(node.dest, context);
          if (!to) return label;
          return to.external
            ? `<a href="${escape(to.href)}" data-external title="${escape(to.href)}">${label}</a>`
            : `<a href="${escape(to.href)}">${label}</a>`;
        }
        default:
          return escape(node.value);
      }
    }).join('');
  }

  /**
   * GitHub's heading ids, which the guide's own links use: the text in lower
   * case, without punctuation other than `-` and `_`, with each space a `-`. A
   * repeated id gets `-1`, `-2` and so on, so the second "outline" is `outline-1`.
   */
  function slugger() {
    const seen = new Map();
    return (text) => {
      const base = text.toLowerCase().replace(/[^\p{L}\p{M}\p{N}\p{Pc} -]/gu, '').replace(/ /g, '-');
      let slug = base;
      while (seen.has(slug)) {
        const count = seen.get(base) + 1;
        seen.set(base, count);
        slug = `${base}-${count}`;
      }
      seen.set(slug, 0);
      return slug;
    };
  }

  function blocksHtml(blocks, context, tight = false) {
    return blocks.map((block) => blockHtml(block, context, tight)).join('\n');
  }

  function blockHtml(block, context, tight) {
    switch (block.type) {
      case 'heading': {
        const nodes = parseInline(block.text);
        const text = plain(nodes);
        const id = context.idPrefix + context.slug(text);
        context.headings.push({ level: block.level, id, text });
        return `<h${block.level} id="${escape(id)}">${inlineHtml(nodes, context)}</h${block.level}>`;
      }
      case 'paragraph': {
        const html = inlineHtml(parseInline(block.text), context);
        return tight ? html : `<p>${html}</p>`;
      }
      case 'code':
        return `<pre><code>${escape(block.text)}</code></pre>`;
      case 'quote':
        return `<blockquote>${blocksHtml(block.children, context)}</blockquote>`;
      case 'rule':
        return '<hr>';
      case 'list': {
        const tag = block.ordered ? 'ol' : 'ul';
        const start = block.ordered && block.start !== 1 ? ` start="${block.start}"` : '';
        const items = block.items.map((item) => `<li>${blocksHtml(item, context, !block.loose)}</li>`).join('');
        return `<${tag}${start}>${items}</${tag}>`;
      }
      case 'table': {
        const cell = (tag, text, align) =>
          `<${tag}${align ? ` style="text-align: ${align}"` : ''}>${inlineHtml(parseInline(text), context)}</${tag}>`;
        const head = `<thead><tr>${block.head.map((text, c) => cell('th', text, block.align[c])).join('')}</tr></thead>`;
        const rows = block.rows.map((row) => `<tr>${row.map((text, c) => cell('td', text, block.align[c])).join('')}</tr>`);
        const body = rows.length ? `<tbody>${rows.join('')}</tbody>` : '';
        // The wrapper scrolls a wide table sideways instead of widening the page.
        return `<div class="table"><table>${head}${body}</table></div>`;
      }
      default:
        return '';
    }
  }

  /**
   * The HTML of `text` and its headings, `{ html, headings }`, each heading
   * `{ level, id, text }` in reading order. `base` is the address relative
   * links resolve against; without it they are plain text. `idPrefix` goes
   * before every heading id and every `#` link, so the ids cannot clash with
   * the page's own.
   */
  function render(text, { base = null, idPrefix = '' } = {}) {
    const lines = String(text).split(/\r\n|\r|\n/).map(expandTabs);
    const context = { base, idPrefix, slug: slugger(), headings: [] };
    const html = blocksHtml(parseBlocks(lines), context);
    return { html, headings: context.headings };
  }

  /**
   * The contents list: the headings of levels `from` to `to` as a tree of
   * `{ id, text, level, children }`. A heading is the child of the nearest
   * heading before it of a lower level, so a skipped level nests as one step.
   */
  function contents(headings, { from = 2, to = 3 } = {}) {
    const root = { level: from - 1, children: [] };
    const open = [root];
    for (const { id, text, level } of headings) {
      if (level < from || level > to) continue;
      while (open[open.length - 1].level >= level) open.pop();
      const entry = { id, text, level, children: [] };
      open[open.length - 1].children.push(entry);
      open.push(entry);
    }
    return root.children;
  }

  return { contents, render };
})();

if (typeof module === 'object') module.exports = markdown;
