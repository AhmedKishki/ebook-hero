'use strict';
// The model behind the window's Book view: the book as `ebook-hero extract` writes
// it (the Document), and which of its blocks a passage covers, as `ebook-hero read`
// names them. It has no Electron in it, so `node --test` can exercise it without a
// display. A reader's own state, such as a position or a bookmark, would belong
// beside this, not in the page.

const { execBinary, failure, locateBook } = require('./runner');

// A later schema only adds fields (see AGENTS.md), so this version and newer read the same.
const MIN_SCHEMA_VERSION = 2;
// How many books stay extracted at once.
const KEPT_BOOKS = 2;

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
 * The ids of the blocks that `passage` covers, which `read --json` names in the
 * `blocks` of each entry of its `passages`. Throws BookError when the binary names
 * none, or names one the extracted book does not have.
 */
function blockIds(document, passage) {
  const ids = passage?.blocks;
  if (!Array.isArray(ids) || ids.length === 0) {
    throw new BookError(
      `passage ${passage?.passage_id} names no blocks in the output of "ebook-hero read"; rebuild the binary with "cargo build --release"`,
    );
  }
  const known = new Set(document.blocks.map((block) => block.id));
  const missing = ids.find((id) => !known.has(id));
  if (missing !== undefined) {
    throw new BookError(
      `passage ${passage.passage_id} names block ${missing}, which the extracted book does not have; the book may have changed since it was indexed, `
        + 'so run "ebook-hero cache clear" and try again',
    );
  }
  return ids;
}

/**
 * The books the window has open. `exec(args)` runs the binary; tests give it a
 * fake. A book is extracted once per version of its file.
 */
function createLibrary(exec = execBinary) {
  const books = new Map();

  async function load(file) {
    const where = locateBook(file);
    if (!where.ok) return where;
    const { absolute, key } = where;
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
   * Opens the book at `path`. Resolves to `{ ok, key, view?, ids? }`: `view` is
   * left out when `have` is already the key of that book, and `ids`, the ids of
   * the passage's blocks, come with `passage`. A failure is `{ ok: false, stderr }`.
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
      reply.ids = blockIds(loaded.book.document, result.passages?.[0]);
    } catch (error) {
      if (!(error instanceof BookError) && !(error instanceof SyntaxError)) throw error;
      return failure(`error: ${error.message}`);
    }
    return reply;
  }

  return { open };
}

module.exports = { BookError, createLibrary, parseDocument, viewOf };
