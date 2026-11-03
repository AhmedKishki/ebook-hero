'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { test } = require('node:test');
const { BookError, createLibrary, parseDocument, passageBlocks, viewOf } = require('./book');
const { findBinary } = require('./runner');

const FIXTURES = path.join(__dirname, '..', 'tests', 'fixtures');
const LONG = 'A long paragraph that goes on well past forty characters, so that a piece of it can be told from a heading.';

function epubBlock(id, para, text, href = 'ch1.xhtml', type = 'paragraph') {
  return { id, kind: { type, ...(type === 'heading' ? { level: 2 } : {}) }, text, section: [], locator: { format: 'epub', href, para } };
}

function pdfBlock(id, page, text, pageEnd = null) {
  return { id, kind: { type: 'paragraph' }, text, section: [], locator: { format: 'pdf', page, page_end: pageEnd } };
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

test('an EPUB passage covers the blocks of its file and paragraph range', () => {
  const doc = document([
    epubBlock(0, 1, 'Heading', 'ch1.xhtml', 'heading'),
    epubBlock(1, 2, 'one'),
    epubBlock(2, 3, 'two'),
    epubBlock(3, 4, 'three'),
    epubBlock(4, 2, 'same paragraph number, other file', 'ch2.xhtml'),
  ]);
  const passage = { passage_id: 7, locator: { format: 'epub', href: 'ch1.xhtml', para_start: 2, para_end: 3 } };
  assert.deepEqual(passageBlocks(doc, passage), { ids: [1, 2], precise: true });
});

test('a PDF passage covers the blocks on its pages whose paragraphs it holds', () => {
  const doc = document([
    pdfBlock(0, 1, 'Before the passage, on the same page as its start.'),
    pdfBlock(1, 1, 'First paragraph of the passage, and its heading is above.'),
    pdfBlock(2, 2, 'Second paragraph, on the next page.'),
    pdfBlock(3, 2, 'After the passage.'),
    pdfBlock(4, 3, 'Out of the page range.'),
  ]);
  const passage = { passage_id: 1, locator: { format: 'pdf', page_start: 1, page_end: 2 } };
  const markdown = [
    '## Section',
    '<!-- passage 1 · p. 1–2 -->',
    'First paragraph of the passage,\nand its heading is above.',
    'Second paragraph, on the next page.',
  ].join('\n\n');
  assert.deepEqual(passageBlocks(doc, passage, markdown), { ids: [1, 2], precise: true });
});

test('a short heading is not in a passage just because its word is', () => {
  const doc = document([
    pdfBlock(0, 1, 'Entropy'),
    pdfBlock(1, 1, 'Entropy measures how much surprise a source of messages carries.'),
  ]);
  const passage = { passage_id: 1, locator: { format: 'pdf', page_start: 1, page_end: 1 } };
  const markdown = '## Entropy\n\n<!-- passage 1 · p. 1 -->\n\nEntropy measures how much surprise a source of messages carries.\n';
  assert.deepEqual(passageBlocks(doc, passage, markdown), { ids: [1], precise: true });
});

test('a passage that is a piece of one long block marks that block', () => {
  const doc = document([pdfBlock(0, 1, `${LONG} Second sentence of the block.`), pdfBlock(1, 1, 'Another block.')]);
  const passage = { passage_id: 1, locator: { format: 'pdf', page_start: 1, page_end: 1 } };
  const markdown = `<!-- passage 1 · p. 1 -->\n\n${LONG}\n`;
  assert.deepEqual(passageBlocks(doc, passage, markdown), { ids: [0], precise: true });
});

test('a block that spans pages is on each of them', () => {
  const doc = document([pdfBlock(0, 1, LONG, 2), pdfBlock(1, 3, 'Elsewhere.')]);
  const passage = { passage_id: 1, locator: { format: 'pdf', page_start: 2, page_end: 2 } };
  assert.deepEqual(passageBlocks(doc, passage, `<!-- passage 1 -->\n\n${LONG}`), { ids: [0], precise: true });
});

test('when no PDF block can be matched, the whole page range is marked and not precise', () => {
  const doc = document([pdfBlock(0, 1, 'One.'), pdfBlock(1, 2, 'Two.'), pdfBlock(2, 3, 'Three.')]);
  const passage = { passage_id: 1, locator: { format: 'pdf', page_start: 1, page_end: 2 } };
  assert.deepEqual(passageBlocks(doc, passage, '<!-- passage 1 -->\n\nSomething the blocks do not say, differently.'), { ids: [0, 1], precise: false });
  assert.deepEqual(passageBlocks(doc, passage), { ids: [0, 1], precise: false });
});

test('a passage that matches nothing, or has a locator this window does not know, is an error with a remedy', () => {
  const doc = document([epubBlock(0, 1, 'x')]);
  assert.throws(
    () => passageBlocks(doc, { passage_id: 9, locator: { format: 'epub', href: 'gone.xhtml', para_start: 1, para_end: 2 } }),
    /passage 9 matches no block.*cache clear/,
  );
  assert.throws(
    () => passageBlocks(doc, { passage_id: 9, locator: { format: 'mobi' } }),
    /format "mobi".*update this window/,
  );
});

function tempBook(contents = 'book') {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'ebook-hero-gui-'));
  const file = path.join(dir, 'b.epub');
  fs.writeFileSync(file, contents);
  return { dir, file };
}

function fakeExec(calls) {
  const doc = document([epubBlock(0, 1, 'one'), epubBlock(1, 2, 'two')]);
  return async (args) => {
    calls.push(args[0]);
    if (args[0] === 'extract') return { ok: true, stdout: JSON.stringify(doc), stderr: '' };
    const passage = { passage_id: 5, locator: { format: 'epub', href: 'ch1.xhtml', para_start: 2, para_end: 2 } };
    return { ok: true, stdout: JSON.stringify({ markdown: '', passages: [passage] }), stderr: '' };
  };
}

test('a book is extracted once per version of its file, and its view is sent only when asked for', async () => {
  const { dir, file } = tempBook();
  try {
    const calls = [];
    const library = createLibrary(fakeExec(calls));
    const first = await library.open({ path: file });
    assert.equal(first.ok, true);
    assert.equal(first.view.blocks.length, 2);
    const second = await library.open({ path: file, passage: 5, have: first.key });
    assert.deepEqual({ ok: second.ok, ids: second.ids, precise: second.precise, view: second.view }, { ok: true, ids: [1], precise: true, view: undefined });
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
  assert.equal(opened.precise, true);
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
    assert.equal(opened.precise, true, `passage ${passage}`);
  }
});

test('the real binary: a passage that does not exist is the binary\'s own error', { skip: !built }, async () => {
  const opened = await createLibrary().open({ path: path.join(FIXTURES, 'alice.epub'), passage: 99999 });
  assert.equal(opened.ok, false);
  assert.match(opened.stderr, /passage 99999 does not exist/);
});
