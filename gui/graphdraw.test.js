'use strict';

const assert = require('node:assert/strict');
const { test } = require('node:test');
const graphDraw = require('./graphdraw');

// Five concepts in two communities: queen, king and court in the first, hatter
// and dormouse in the second, with one link between the two communities.
function twoCommunities() {
  const concept = (id, label, passages, community) => ({
    id, label, passage_count: passages, passages: Array.from({ length: passages }, (_, p) => p), community,
  });
  return {
    passages: 40,
    concepts: [
      concept(0, 'queen', 9, 0),
      concept(1, 'king', 4, 0),
      concept(2, 'court', 6, 0),
      concept(3, 'hatter', 5, 1),
      concept(4, 'dormouse', 3, 1),
    ],
    links: [
      { a: 0, b: 1, npmi: 0.6, passages: [0] },
      { a: 1, b: 2, npmi: 0.4, passages: [1] },
      { a: 2, b: 3, npmi: 0.3, passages: [2] },
      { a: 3, b: 4, npmi: 0.8, passages: [3] },
    ],
    communities: [{ id: 0, concepts: [0, 1, 2] }, { id: 1, concepts: [3, 4] }],
  };
}

const corners = [{ x: 0, y: 0 }, { x: 0.5, y: 0.2 }, { x: 1, y: 0.5 }, { x: 0.3, y: 1 }, { x: 0.8, y: 0.9 }];

test('a community lights its own concepts and only the links inside it', () => {
  const lit = graphDraw.community(twoCommunities(), 0);
  assert.deepEqual([...lit.concepts].sort(), [0, 1, 2]);
  assert.deepEqual([...lit.links].sort(), [0, 1]);
});

test('a community is named by its number from 1 and its four concepts with most passages', () => {
  const graph = twoCommunities();
  graph.concepts.push({ id: 5, label: 'jury', passage_count: 2, passages: [0, 1], community: 0 });
  graph.communities[0].concepts.push(5);
  assert.equal(graphDraw.communityLabel(graph, graph.communities[0]), '1 · queen, court, king, jury');
  assert.equal(graphDraw.communityLabel(graph, graph.communities[1]), '2 · hatter, dormouse');
});

test('the image draws every concept with its label and a fill colour of its own', () => {
  const { svg } = graphDraw.image(twoCommunities(), corners);
  for (const label of ['queen', 'king', 'court', 'hatter', 'dormouse']) {
    assert.match(svg, new RegExp(`>${label}</text>`));
  }
  const fills = [...svg.matchAll(/<circle [^>]*fill="(#[0-9a-f]{6})"/g)].map((m) => m[1]);
  assert.ok(fills.length >= 5, svg);
  assert.ok(!/class=/.test(svg), 'the image must not depend on the window\'s stylesheet');
});

test('the image escapes labels so the file stays well-formed', () => {
  const graph = twoCommunities();
  graph.concepts[0].label = 'R&D <core> "x"';
  const { svg } = graphDraw.image(graph, corners);
  assert.match(svg, /R&amp;D &lt;core&gt; &quot;x&quot;/);
  assert.ok(!svg.includes('<core>'));
});

test('the label of the rightmost concept ends inside the image', () => {
  const graph = twoCommunities();
  graph.concepts[2].label = 'a rather long concept label at the edge';
  const { svg, width } = graphDraw.image(graph, corners);
  const text = svg.match(/<text x="([\d.]+)"[^>]*>a rather long concept label at the edge<\/text>/);
  const size = Number(svg.match(/font-size="([\d.]+)"/)[1]);
  assert.ok(text, svg);
  // Georgia's lowercase letters and spaces average about half an em.
  assert.ok(Number(text[1]) + 39 * 0.5 * size <= width, `${text[1]} + label > ${width}`);
});

test('the legend in the image names every community', () => {
  const { svg } = graphDraw.image(twoCommunities(), corners);
  assert.match(svg, />1 · queen, court, king<\/text>/);
  assert.match(svg, />2 · hatter, dormouse<\/text>/);
});

test('scale enlarges the image in pixels and keeps its drawing', () => {
  const plain = graphDraw.image(twoCommunities(), corners);
  const double = graphDraw.image(twoCommunities(), corners, { scale: 2 });
  assert.match(double.svg, new RegExp(`width="${plain.width * 2}" height="${plain.height * 2}" viewBox="0 0 ${plain.width} ${plain.height}"`));
  assert.equal(double.svg.replace(/^<svg[^>]*>/, ''), plain.svg.replace(/^<svg[^>]*>/, ''));
});

test('an empty graph still gives an image', () => {
  const { svg } = graphDraw.image({ passages: 0, concepts: [], links: [], communities: [] }, []);
  assert.match(svg, /^<svg [^>]*>.*<\/svg>$/s);
  assert.ok(!svg.includes('NaN') && !svg.includes('Infinity'), svg);
});
