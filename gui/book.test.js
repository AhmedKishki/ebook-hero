'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { test } = require('node:test');
const { BookError, createLibrary, parseDocument, viewOf } = require('./book');
const { findBinary } = require('./runner');

const FIXTURES = path.join(__dirname, '..', 'tests', 'fixtures');

function epubBlock(id, para, text, href = 'ch1.xhtml', type = 'paragraph') {
  return { id, kind: { type, ...(type === 'heading' ? { level: 2 } : {}) }, text, section: [], locator: { format: 'epub', href, para } };
}

function document(blocks, extra = {}) {
  return { schema_version: 2, source: { format: 'epub' }, metadata: { title: 'T' }, blocks, ...extra };
}

test('a Document of this schema or a later one is read; an older or broken one is refused with a remedy', () => {
  assert.equal(parseDocument(JSON.stringify(document([]))).schema_version, 2);
  assert.equal(parseDocument(JSON.stringify(document([], { schema_version: 3 }))).schema_version, 3);
  assert.throws(() => parseDocument(JSON.stringify(document([], { schema_version: 1 }))), /schema 1.*schema 2 or later.*cargo build --release/);
  assert.throws(() => parseDocument(JSON.stringify({ blocks: [] })), /schema undefined/);
  assert.throws(() => parseDocument('not json'), (error) => error instanceof BookError && /not JSON.*cargo build --release/.test(error.message));
});

test('the view keeps the text in order and drops the locators', () => {
  const view = viewOf(document([epubBlock(0, 1, 'Title', 'a', 'heading'), epubBlock(1, 2, 'Body')]));
  assert.deepEqual(view, {
    title: 'T',
    format: 'epub',
    blocks: [
      { id: 0, type: 'heading', level: 2, text: 'Title' },
      { id: 1, type: 'paragraph', level: null, text: 'Body' },
    ],
  });
});

function tempBook(contents = 'book') {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'ebook-hero-gui-'));
  const file = path.join(dir, 'b.epub');
  fs.writeFileSync(file, contents);
  return { dir, file };
}

/** A fake binary: `extract` writes `blocks`, and `read` writes one passage with `passage`'s fields. */
function fakeExec(calls, { blocks = [epubBlock(0, 1, 'one'), epubBlock(1, 2, 'two')], passage = { blocks: [1] } } = {}) {
  return async (args) => {
    calls.push(args[0]);
    if (args[0] === 'extract') return { ok: true, stdout: JSON.stringify(document(blocks)), stderr: '' };
    const read = { passage_id: 5, section: [], locator: { format: 'epub' }, ...passage };
    return { ok: true, stdout: JSON.stringify({ markdown: '', passages: [read] }), stderr: '' };
  };
}

test('a passage marks the blocks read --json names', async () => {
  const { dir, file } = tempBook();
  try {
    const exec = fakeExec([], {
      blocks: [epubBlock(0, 1, 'one'), epubBlock(1, 2, 'two'), epubBlock(2, 3, 'three')],
      passage: { blocks: [1, 2] },
    });
    const reply = await createLibrary(exec).open({ path: file, passage: 5 });
    assert.equal(reply.ok, true);
    assert.deepEqual(reply.ids, [1, 2]);
    assert.equal('precise' in reply, false);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('a passage whose blocks are missing or not in the book is an error with a remedy', async () => {
  const { dir, file } = tempBook();
  try {
    const open = (passage) => createLibrary(fakeExec([], { passage })).open({ path: file, passage: 5 });
    const old = await open({ blocks: undefined });
    assert.equal(old.ok, false);
    assert.match(old.stderr, /passage 5 .*no blocks.*cargo build --release/);
    assert.match((await open({ blocks: [] })).stderr, /passage 5 .*no blocks/);
    assert.match((await open({ blocks: [1, 9] })).stderr, /passage 5 .*block 9.*cache clear/);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('a book is extracted once per version of its file, and its view is sent only when asked for', async () => {
  const { dir, file } = tempBook();
  try {
    const calls = [];
    const library = createLibrary(fakeExec(calls));
    const first = await library.open({ path: file });
    assert.equal(first.ok, true);
    assert.equal(first.view.blocks.length, 2);
    const second = await library.open({ path: file, passage: 5, have: first.key });
    assert.deepEqual({ ok: second.ok, ids: second.ids, view: second.view }, { ok: true, ids: [1], view: undefined });
    assert.deepEqual(calls, ['extract', 'read']);
    const other = await library.open({ path: file, have: 'a key of another book' });
    assert.equal(other.view.blocks.length, 2);
    assert.deepEqual(calls, ['extract', 'read']);

    fs.writeFileSync(file, 'a different book, longer');
    const changed = await library.open({ path: file, have: first.key });
    assert.notEqual(changed.key, first.key);
    assert.ok(changed.view);
    assert.deepEqual(calls, ['extract', 'read', 'extract']);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('opening a book that cannot be opened says why', async () => {
  const { dir, file } = tempBook();
  try {
    const library = createLibrary(fakeExec([]));
    assert.match((await library.open({ path: path.join(dir, 'missing.epub') })).stderr, /cannot read .*missing\.epub \(ENOENT\); check the path/);
    assert.match((await library.open({ path: dir })).stderr, /is not a file; type the path of a PDF or EPUB/);
    assert.match((await library.open({ path: '  ' })).stderr, /the path is empty; type the path.*Browse/);
    assert.match((await library.open({ path: file, passage: -1 })).stderr, /a passage is a whole number/);
    assert.match((await library.open({ path: file, passage: '5' })).stderr, /a passage is a whole number/);
    const refused = createLibrary(async () => ({ ok: false, stdout: '', stderr: 'error: not a book\n', binary: 'b' }));
    assert.deepEqual(await refused.open({ path: file }), { ok: false, stdout: '', stderr: 'error: not a book\n', binary: 'b' });
    const stale = createLibrary(async () => ({ ok: true, stdout: JSON.stringify({ schema_version: 1, blocks: [] }), stderr: '' }));
    assert.match((await stale.open({ path: file })).stderr, /schema 1.*cargo build --release/);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

// These need a binary (`cargo build --release`, or one on the PATH); neither needs the models.
const built = findBinary() !== null;

test('the real binary: an EPUB passage is exactly its paragraphs', { skip: !built }, async () => {
  const library = createLibrary();
  const opened = await library.open({ path: path.join(FIXTURES, 'alice.epub'), passage: 95 });
  assert.equal(opened.ok, true, opened.stderr);
  assert.equal(opened.view.blocks.length, 875);
  assert.equal(opened.view.title, "Alice's Adventures in Wonderland");
  assert.deepEqual(opened.ids, [452, 453, 454, 455, 456, 457, 458]);
  assert.match(opened.view.blocks[452].text, /^When the procession came opposite to Alice/);
});

test('the real binary: a PDF passage is exactly its paragraphs, across a page break', { skip: !built }, async () => {
  const library = createLibrary();
  const file = path.join(FIXTURES, 'sample.pdf');
  const expected = { 1: [4, 5, 6], 2: [8, 9, 10], 5: [17, 18, 19] };
  for (const [passage, ids] of Object.entries(expected)) {
    const opened = await library.open({ path: file, passage: Number(passage) });
    assert.equal(opened.ok, true, opened.stderr);
    assert.deepEqual(opened.ids, ids, `passage ${passage}`);
  }
});

test('the real binary: a passage that does not exist is the binary\'s own error', { skip: !built }, async () => {
  const opened = await createLibrary().open({ path: path.join(FIXTURES, 'alice.epub'), passage: 99999 });
  assert.equal(opened.ok, false);
  assert.match(opened.stderr, /passage 99999 does not exist/);
});
