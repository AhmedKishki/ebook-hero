'use strict';
// The model behind the window's Book view: the book as `ebook-hero extract` writes
// it (the Document), and which of its blocks a passage covers. It has no Electron
// in it, so `node --test` can exercise it without a display. A reader's own state,
// such as a position or a bookmark, would belong beside this, not in the page.

const fs = require('node:fs');
const path = require('node:path');
const { execBinary, expandHome, failure } = require('./runner');

// A later schema only adds fields (see AGENTS.md), so this version and newer read the same.
const MIN_SCHEMA_VERSION = 2;
// How many books stay extracted at once.
const KEPT_BOOKS = 2;
// A paragraph of this many characters or more can be matched as a piece of a longer block.
const PROBE = 40;

class BookError extends Error {}

/** The Document in `text`. Throws BookError, naming what to rebuild or update. */
function parseDocument(text) {
  let document;
  try {
    document = JSON.parse(text);
  } catch (error) {
    throw new BookError(`the output of "ebook-hero extract" is not JSON (${error.message}); rebuild the binary with "cargo build --release"`);
  }
  const version = document?.schema_version;
  if (!Number.isInteger(version) || version < MIN_SCHEMA_VERSION || !Array.isArray(document.blocks)) {
    throw new BookError(
      `the binary wrote a Document of schema ${version}, and this window reads schema ${MIN_SCHEMA_VERSION} or later; `
        + 'rebuild the binary with "cargo build --release", or update this window',
    );
  }
  return document;
}

/** What the page needs to draw the book: its text in order, without locators. */
function viewOf(document) {
  return {
    title: document.metadata?.title ?? null,
    format: document.source?.format ?? null,
    blocks: document.blocks.map((block) => ({
      id: block.id,
      type: block.kind.type,
      level: block.kind.level ?? null,
      text: block.text,
    })),
  };
}

/**
 * The ids of the blocks that passage `passage` covers, and whether they are
 * exact. `passage` is an entry of `read --json`'s `passages`; `markdown` is that
 * result's text.
 *
 * An EPUB passage names its chapter file and a range of paragraphs, and so does
 * each block: the match is exact. A PDF passage names only a range of pages, so
 * the blocks on those pages are narrowed to those whose text is in the passage;
 * when none is found, all blocks on the pages are returned with `precise: false`.
 */
function passageBlocks(document, passage, markdown = '') {
  const locator = passage?.locator;
  let found;
  let precise = true;
  if (locator?.format === 'epub') {
    found = document.blocks.filter(({ locator: at }) =>
      at.href === locator.href && at.para >= locator.para_start && at.para <= locator.para_end);
  } else if (locator?.format === 'pdf') {
    const onPages = document.blocks.filter(({ locator: at }) =>
      at.page <= locator.page_end && (at.page_end ?? at.page) >= locator.page_start);
    const paragraphs = bodyOf(markdown).split(/\n\s*\n/).map(normalize).filter(Boolean);
    const inside = onPages.filter((block) => inPassage(normalize(block.text), paragraphs));
    precise = inside.length > 0;
    found = precise ? inside : onPages;
  } else {
    throw new BookError(`passage ${passage?.passage_id} has a locator of format "${locator?.format}", which this window cannot place; update this window`);
  }
  if (found.length === 0) {
    throw new BookError(
      `passage ${passage.passage_id} matches no block of the extracted book; the book may have changed since it was indexed, `
        + 'so run "ebook-hero cache clear" and try again',
    );
  }
  return { ids: found.map((block) => block.id), precise };
}

/** The passage's own text: what follows the `<!-- passage … -->` line of a `read` result. */
function bodyOf(markdown) {
  const end = markdown.indexOf('-->');
  return end < 0 ? markdown : markdown.slice(end + 3);
}

function normalize(text) {
  return text.replace(/\s+/g, ' ').trim();
}

/**
 * Whether a block is in a passage, given the passage's paragraphs. A passage is
 * whole blocks, or a piece of one long block, so a block counts when it is one
 * of the paragraphs or contains one. A short paragraph must be the whole block:
 * the heading "Entropy" is not in a passage just because the word is in it.
 */
function inPassage(blockText, paragraphs) {
  if (!blockText) return false;
  return paragraphs.some((paragraph) =>
    paragraph === blockText || (paragraph.length >= PROBE && blockText.includes(paragraph)));
}

/**
 * The books the window has open. `exec(args)` runs the binary; tests give it a
 * fake. A book is extracted once per version of its file.
 */
function createLibrary(exec = execBinary) {
  const books = new Map();

  async function load(file) {
    const given = file.trim();
    if (!given) {
      return failure('error: the path is empty; type the path of a PDF or EPUB, or press Browse');
    }
    let absolute;
    let stat;
    try {
      absolute = path.resolve(expandHome(given));
      stat = fs.statSync(absolute);
    } catch (error) {
      return failure(`error: cannot read ${given} (${error.code ?? error.message}); check the path`);
    }
    if (!stat.isFile()) {
      return failure(`error: ${given} is not a file; type the path of a PDF or EPUB`);
    }
    const key = `${absolute}|${stat.size}|${stat.mtimeMs}`;
    if (!books.has(key)) {
      const result = await exec(['extract', '--format', 'json', '--', absolute]);
      if (!result.ok) return result;
      let document;
      try {
        document = parseDocument(result.stdout);
      } catch (error) {
        if (!(error instanceof BookError)) throw error;
        return failure(`error: ${error.message}`);
      }
      books.set(key, { document, view: viewOf(document) });
      while (books.size > KEPT_BOOKS) books.delete(books.keys().next().value);
    }
    return { ok: true, key, absolute, book: books.get(key) };
  }

  /**
   * Opens the book at `path`. Resolves to `{ ok, key, view?, ids?, precise? }`:
   * `view` is left out when `have` is already the key of that book, and `ids`
   * and `precise` come with `passage`. A failure is `{ ok: false, stderr }`.
   */
  async function open({ path: file, passage, have } = {}) {
    const loaded = await load(String(file ?? ''));
    if (!loaded.ok) return loaded;
    const reply = { ok: true, key: loaded.key };
    if (loaded.key !== have) reply.view = loaded.book.view;
    if (passage === undefined) return reply;
    if (!Number.isInteger(passage) || passage < 0) {
      return failure(`error: a passage is a whole number, such as 12; ${JSON.stringify(passage)} is not`);
    }
    const read = await exec(['read', '--json', '--', loaded.absolute, String(passage)]);
    if (!read.ok) return read;
    let result;
    try {
      result = JSON.parse(read.stdout);
      Object.assign(reply, passageBlocks(loaded.book.document, result.passages?.[0], result.markdown));
    } catch (error) {
      if (!(error instanceof BookError) && !(error instanceof SyntaxError)) throw error;
      return failure(`error: ${error.message}`);
    }
    return reply;
  }

  return { open };
}

module.exports = { BookError, createLibrary, parseDocument, passageBlocks, viewOf };
