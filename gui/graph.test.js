'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { test } = require('node:test');
const { GraphError, createGraphs, layout, parseGraph } = require('./graph');

function graphOf(concepts, links) {
  return {
    passages: 30,
    topics: 5,
    concepts: concepts.map((label, id) => ({ id, label, passages: [id], community: 0 })),
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

test('open runs graph --json on the absolute path once for an unchanged file', async () => {
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
  assert.deepEqual(calls, [['graph', '--json', '--', book]]);
  fs.rmSync(dir, { recursive: true });
});
