'use strict';
// The model behind the window's Graph view: the concept graph as `ebook-hero
// graph --json` writes it, and where to draw each concept. It has no Electron
// in it, so `node --test` can exercise it without a display.

const { execBinary, failure, locateBook } = require('./runner');

// How many books' graphs stay loaded at once.
const KEPT_GRAPHS = 2;
// How far, as a share of the drawing, a concept may move in the first step of
// the layout; the step shrinks to nothing by the last.
const START_TEMPERATURE = 0.1;
// The ideal distance between concepts is this over the square root of their
// number, as in the spike's networkx layout: wide enough for their labels.
const SPREAD = 1.6;
// Each concept is also pulled toward the middle of all of them, this hard per
// unit of distance, so a concept without links stays near the rest instead of
// drifting off and leaving them in a corner once the drawing is scaled to fit.
const GRAVITY = 1;

class GraphError extends Error {}

/**
 * The concept graph in `text`. Throws GraphError, naming the first field this
 * window reads that the result lacks, when the binary is older or newer than
 * the window.
 */
function parseGraph(text) {
  const graph = JSON.parse(text);
  const need = (present, field) => {
    if (!present) {
      throw new GraphError(`the concept graph has no ${field}; update ebook-hero and this window to the same version`);
    }
  };
  need(Number.isInteger(graph?.passages), 'passages');
  for (const list of ['concepts', 'links', 'communities']) need(Array.isArray(graph[list]), list);
  for (const c of graph.concepts) for (const f of ['id', 'label', 'passage_count', 'passages', 'community']) need(c[f] !== undefined, `concepts[].${f}`);
  for (const l of graph.links) for (const f of ['a', 'b', 'npmi', 'passages']) need(l[f] !== undefined, `links[].${f}`);
  for (const c of graph.communities) for (const f of ['id', 'concepts']) need(c[f] !== undefined, `communities[].${f}`);
  return graph;
}

/**
 * Where to draw each concept: `{ x, y }` in [0, 1]², by Fruchterman and
 * Reingold's spring layout. Linked concepts pull together, harder the higher
 * their NPMI, every concept pushes every other away, and a weak pull toward
 * the middle keeps unlinked concepts close. Nothing holds the concepts inside a
 * frame while they settle, as nothing does in networkx's version; the result
 * is scaled to fit afterwards, so no concept is pinned to an edge. The start
 * comes from `seed`, so the same graph is always drawn the same way.
 */
function layout(graph, { seed = 1, iterations = 300 } = {}) {
  const n = graph.concepts.length;
  const random = mulberry32(seed);
  const pos = Array.from({ length: n }, () => ({ x: random(), y: random() }));
  const k = SPREAD / Math.sqrt(Math.max(n, 1));
  const links = graph.links.filter((l) => l.a < n && l.b < n && l.a !== l.b);
  for (let step = 0; step < iterations; step++) {
    const move = pos.map(() => ({ x: 0, y: 0 }));
    for (let i = 0; i < n; i++) {
      for (let j = i + 1; j < n; j++) {
        const dx = pos[i].x - pos[j].x;
        const dy = pos[i].y - pos[j].y;
        const d = Math.hypot(dx, dy) || 1e-6;
        const push = (k * k) / d;
        move[i].x += (dx / d) * push; move[i].y += (dy / d) * push;
        move[j].x -= (dx / d) * push; move[j].y -= (dy / d) * push;
      }
    }
    for (const { a, b, npmi } of links) {
      const dx = pos[a].x - pos[b].x;
      const dy = pos[a].y - pos[b].y;
      const d = Math.hypot(dx, dy) || 1e-6;
      const pull = ((d * d) / k) * npmi;
      move[a].x -= (dx / d) * pull; move[a].y -= (dy / d) * pull;
      move[b].x += (dx / d) * pull; move[b].y += (dy / d) * pull;
    }
    const middle = {
      x: pos.reduce((sum, p) => sum + p.x, 0) / n,
      y: pos.reduce((sum, p) => sum + p.y, 0) / n,
    };
    for (let i = 0; i < n; i++) {
      move[i].x -= GRAVITY * (pos[i].x - middle.x);
      move[i].y -= GRAVITY * (pos[i].y - middle.y);
    }
    const temperature = START_TEMPERATURE * (1 - step / iterations);
    for (let i = 0; i < n; i++) {
      const d = Math.hypot(move[i].x, move[i].y);
      if (d === 0) continue;
      const stride = Math.min(d, temperature);
      pos[i].x += (move[i].x / d) * stride;
      pos[i].y += (move[i].y / d) * stride;
    }
  }
  return normalised(pos);
}

/** The positions stretched to fill [0, 1]² on each axis, or centred on an axis where they do not spread. */
function normalised(pos) {
  const span = (values) => [Math.min(...values), Math.max(...values)];
  const [x0, x1] = span(pos.map((p) => p.x));
  const [y0, y1] = span(pos.map((p) => p.y));
  return pos.map((p) => ({
    x: x1 > x0 ? (p.x - x0) / (x1 - x0) : 0.5,
    y: y1 > y0 ? (p.y - y0) / (y1 - y0) : 0.5,
  }));
}

/** Mulberry32: a small generator whose numbers in [0, 1) depend only on `seed`. */
function mulberry32(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/**
 * The failure to show for a binary from before the graph, or before its
 * `--full` flag, whose own message names neither the cause nor a remedy.
 */
function olderBinary(result) {
  if (!/unrecognized subcommand 'graph'|unexpected argument '--full'/.test(result.stderr ?? '')) return null;
  const binary = result.binary ?? 'this ebook-hero';
  return failure(
    `error: ${binary} has no graph command, or an older one; run "cargo build --release" in the repository root, `
      + 'or set EBOOK_HERO_BIN to an ebook-hero of the same version as this window',
  );
}

/**
 * Loads concept graphs through `exec`, keeping the last few, each with its
 * layout. `open({ path })` resolves to `{ ok, key, absolute, graph, positions }`
 * or to a failure `{ ok: false, stderr }`.
 */
function createGraphs(exec = execBinary) {
  const graphs = new Map();

  async function open({ path: file } = {}) {
    const where = locateBook(file);
    if (!where.ok) return where;
    const { absolute, key } = where;
    if (!graphs.has(key)) {
      const result = await exec(['graph', '--json', '--full', '--', absolute]);
      if (!result.ok) return olderBinary(result) ?? result;
      let graph;
      try {
        graph = parseGraph(result.stdout);
      } catch (error) {
        if (!(error instanceof GraphError) && !(error instanceof SyntaxError)) throw error;
        return failure(`error: ${error.message}`);
      }
      graphs.set(key, { graph, positions: layout(graph) });
      while (graphs.size > KEPT_GRAPHS) graphs.delete(graphs.keys().next().value);
    }
    const { graph, positions } = graphs.get(key);
    return { ok: true, key, absolute, graph, positions };
  }

  return { open };
}

module.exports = { GraphError, createGraphs, layout, parseGraph };
