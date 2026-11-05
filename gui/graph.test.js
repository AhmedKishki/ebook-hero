'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { test } = require('node:test');
const { GraphError, createGraphs, layout, parseGraph, saveImage } = require('./graph');

function graphOf(concepts, links) {
  return {
    passages: 30,
    topics: 5,
    concepts: concepts.map((label, id) => ({ id, label, passage_count: 1, passages: [id], community: 0 })),
    links: links.map(([a, b, npmi]) => ({ a, b, npmi, passages: [a] })),
    communities: [{ id: 0, concepts: concepts.map((_, id) => id) }],
  };
}

test('parseGraph rejects a result without links and says to update', () => {
  const { links: _, ...old } = graphOf(['queen'], []);
  assert.throws(() => parseGraph(JSON.stringify(old)), (error) =>
    error instanceof GraphError && error.message.includes('links') && error.message.includes('update'));
});

test('layout is the same for the same seed, finite and inside the unit square', () => {
  const graph = graphOf(['queen', 'king', 'court', 'jury', 'hatter'], [[0, 1, 0.6], [1, 2, 0.8], [2, 3, 0.7]]);
  const first = layout(graph);
  assert.deepEqual(layout(graph), first);
  for (const { x, y } of first) {
    assert.ok(Number.isFinite(x) && Number.isFinite(y) && x >= 0 && x <= 1 && y >= 0 && y <= 1, `${x}, ${y}`);
  }
});

test('layout keeps a lone concept on the page', () => {
  const [only] = layout(graphOf(['queen'], []));
  assert.ok(Number.isFinite(only.x) && Number.isFinite(only.y));
});

test('open runs graph --json --full on the absolute path once for an unchanged file', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'graph-'));
  const book = path.join(dir, 'book.epub');
  fs.writeFileSync(book, 'x');
  const calls = [];
  const exec = async (args) => {
    calls.push(args);
    return { ok: true, stdout: JSON.stringify(graphOf(['queen', 'king'], [[0, 1, 0.6]])), stderr: '' };
  };
  const graphs = createGraphs(exec);
  const first = await graphs.open({ path: book });
  await graphs.open({ path: book });
  assert.ok(first.ok);
  assert.equal(first.positions.length, 2);
  assert.deepEqual(calls, [['graph', '--json', '--full', '--', book]]);
  fs.rmSync(dir, { recursive: true });
});

test('layout keeps linked concepts spread when one concept has no links', () => {
  const labels = ['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'lone'];
  const ring = [0, 1, 2, 3, 4, 5, 6, 7].map((i) => [i, (i + 1) % 8, 0.5]);
  const linked = layout(graphOf(labels, ring)).slice(0, 8);
  const span = (values) => Math.max(...values) - Math.min(...values);
  assert.ok(span(linked.map((p) => p.x)) > 0.5, JSON.stringify(linked));
  assert.ok(span(linked.map((p) => p.y)) > 0.5, JSON.stringify(linked));
});

test('an older binary without the graph command is named, with how to update it', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'graph-'));
  const book = path.join(dir, 'book.epub');
  fs.writeFileSync(book, 'x');
  const exec = async () => ({
    ok: false, stdout: '', stderr: "error: unrecognized subcommand 'graph'\n", binary: '/old/ebook-hero',
  });
  const reply = await createGraphs(exec).open({ path: book });
  assert.equal(reply.ok, false);
  assert.match(reply.stderr, /\/old\/ebook-hero has no graph command/);
  assert.match(reply.stderr, /cargo build --release|EBOOK_HERO_BIN/);
  fs.rmSync(dir, { recursive: true });
});

/** A folder holding a book, and a dialog that answers `answer` and keeps the name it was offered. */
function saving(answer) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'graph-image-'));
  const book = path.join(dir, 'Alice.epub');
  fs.writeFileSync(book, 'x');
  const offered = [];
  const choose = async (proposed) => {
    offered.push(proposed);
    return typeof answer === 'function' ? answer(dir) : answer;
  };
  return { dir, book, offered, choose };
}

const image = { svg: '<svg xmlns="http://www.w3.org/2000/svg"/>', png: new Uint8Array([0x89, 0x50, 0x4e, 0x47]) };

test('saveImage offers <book>-concepts.png beside the book and writes nothing when cancelled', async () => {
  const { dir, book, offered, choose } = saving(null);
  const reply = await saveImage({ book, ...image }, choose);
  assert.deepEqual(reply, { ok: true, path: null });
  assert.deepEqual(offered, [path.join(dir, 'Alice-concepts.png')]);
  assert.deepEqual(fs.readdirSync(dir), ['Alice.epub']);
  fs.rmSync(dir, { recursive: true });
});

test('saveImage writes the SVG text when the chosen name ends in .svg', async () => {
  const { dir, book, choose } = saving((d) => path.join(d, 'graph.SVG'));
  const reply = await saveImage({ book, ...image }, choose);
  assert.deepEqual(reply, { ok: true, path: path.join(dir, 'graph.SVG') });
  assert.equal(fs.readFileSync(path.join(dir, 'graph.SVG'), 'utf8'), image.svg);
  fs.rmSync(dir, { recursive: true });
});

test('saveImage writes the PNG bytes and adds .png to a name without an extension', async () => {
  const { dir, book, choose } = saving((d) => path.join(d, 'graph'));
  const reply = await saveImage({ book, ...image }, choose);
  assert.deepEqual(reply, { ok: true, path: path.join(dir, 'graph.png') });
  assert.deepEqual([...fs.readFileSync(path.join(dir, 'graph.png'))], [...image.png]);
  fs.rmSync(dir, { recursive: true });
});

test('saveImage refuses another extension, names the two it writes, and writes nothing', async () => {
  const { dir, book, choose } = saving((d) => path.join(d, 'graph.jpg'));
  const reply = await saveImage({ book, ...image }, choose);
  assert.equal(reply.ok, false);
  assert.match(reply.stderr, /\.jpg/);
  assert.match(reply.stderr, /\.png.*\.svg|\.svg.*\.png/);
  assert.deepEqual(fs.readdirSync(dir), ['Alice.epub']);
  fs.rmSync(dir, { recursive: true });
});

test('saveImage names the folder it cannot write to', async () => {
  const { dir, book, choose } = saving((d) => path.join(d, 'missing', 'graph.png'));
  const reply = await saveImage({ book, ...image }, choose);
  assert.equal(reply.ok, false);
  assert.match(reply.stderr, /missing/);
  assert.match(reply.stderr, /ENOENT/);
  fs.rmSync(dir, { recursive: true });
});
