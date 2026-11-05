'use strict';
// How the concept graph is drawn, shared by the Graph pane (graphview.js) and
// the image it saves: each community's colour and name, what lighting a
// community lights, and the whole graph as an SVG that stands on its own. It
// has no DOM in it, so the window loads it as a script and `node --test`
// requires it.

const graphDraw = (() => {
  // One colour per community, in the window's palette; communities past these are grey.
  const COLOURS = ['#f0bf3c', '#43e6cb', '#e8798a', '#8fb8ff', '#b9e46a', '#d39bff', '#ff9f5a', '#7fd6f0'];
  const OTHER = '#8a93a6';
  // The window's colours that the image repeats: its background, text and circle rims.
  const GROUND = '#0a1226';
  const INK = '#f3ead0';
  const RIM = '#0f1b33';
  // The image is this wide whatever the window's size; the graph takes this much of its height, the legend the rest.
  const WIDTH = 1600;
  const GRAPH_HEIGHT = 1000;
  // Labels and legend lines in the image, in its own units.
  const FONT_SIZE = 15;
  const LEGEND_LINE = 24;
  // The circles in the image are this much larger than in the pane, which is drawn about 1000 wide.
  const IMAGE_RADIUS = 1.25;
  // Space kept around the drawing, and between a circle and its label.
  const PAD = 16;
  const LABEL_GAP = 4;
  // An upper estimate of a Georgia character's width in ems, to keep labels inside the image.
  const GLYPH = 0.6;

  const colour = (community) => COLOURS[community] ?? OTHER;

  /** A circle's radius in the pane: 3 to 10, by the square root of its share of the passages of the largest concept. */
  const radius = (concept, most) => 3 + 7 * Math.sqrt(concept.passages.length / most);

  /** The number of passages of the concept that has most, and at least 1. */
  const mostPassages = (graph) => Math.max(1, ...graph.concepts.map((c) => c.passages.length));

  /** `1 · queen, court, king, jury`: a community by its number from 1 and its four concepts with most passages. */
  function communityLabel(graph, community) {
    const top = community.concepts
      .map((id) => graph.concepts[id])
      .sort((p, q) => q.passages.length - p.passages.length)
      .slice(0, 4)
      .map((c) => c.label);
    return `${community.id + 1} · ${top.join(', ')}`;
  }

  /** The concepts of community `c` and the indexes of the links between two of them. */
  function community(graph, c) {
    const concepts = new Set(graph.concepts.filter((concept) => concept.community === c).map((concept) => concept.id));
    const links = new Set();
    graph.links.forEach((link, i) => {
      if (concepts.has(link.a) && concepts.has(link.b)) links.add(i);
    });
    return { concepts, links };
  }

  const ENTITIES = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
  const escape = (text) => String(text).replace(/[&<>"']/g, (ch) => ENTITIES[ch]);
  const at = (value) => value.toFixed(1);

  /**
   * The graph with each concept at its place in `positions` (each in [0, 1]²),
   * and below it the legend, as a standalone SVG: `{ svg, width, height }`.
   * Nothing is dimmed, and every colour and size is written into the file, so
   * it looks the same in any viewer. `scale` multiplies the size in pixels and
   * leaves the drawing as it is, for a sharper PNG.
   */
  function image(graph, positions, { scale = 1 } = {}) {
    const most = mostPassages(graph);
    const radii = graph.concepts.map((c) => IMAGE_RADIUS * radius(c, most));
    const edge = PAD + Math.max(FONT_SIZE, ...radii);
    // The drawing is as wide as it can be while every label, which runs right of its circle, ends inside the image.
    let across = WIDTH - 2 * edge;
    positions.forEach((p, i) => {
      const reach = radii[i] + LABEL_GAP + graph.concepts[i].label.length * GLYPH * FONT_SIZE;
      if (p.x > 0) across = Math.min(across, (WIDTH - PAD - edge - reach) / p.x);
    });
    across = Math.max(across, WIDTH / 2);
    const down = GRAPH_HEIGHT - 2 * edge;
    const x = (p) => edge + p.x * across;
    const y = (p) => edge + p.y * down;
    const height = GRAPH_HEIGHT + graph.communities.length * LEGEND_LINE + PAD;

    const lines = graph.links.map((link) => {
      const [a, b] = [positions[link.a], positions[link.b]];
      return `<line x1="${at(x(a))}" y1="${at(y(a))}" x2="${at(x(b))}" y2="${at(y(b))}" stroke-width="${(IMAGE_RADIUS * (0.8 + 2.4 * link.npmi)).toFixed(2)}"/>`;
    });
    const circles = graph.concepts.map((c, i) =>
      `<circle cx="${at(x(positions[i]))}" cy="${at(y(positions[i]))}" r="${at(radii[i])}" fill="${colour(c.community)}"/>`);
    const labels = graph.concepts.map((c, i) =>
      `<text x="${at(x(positions[i]) + radii[i] + LABEL_GAP)}" y="${at(y(positions[i]) + FONT_SIZE / 3)}">${escape(c.label)}</text>`);
    const legend = graph.communities.map((community, row) => {
      const middle = GRAPH_HEIGHT + row * LEGEND_LINE + LEGEND_LINE / 2;
      return `<circle cx="${at(edge)}" cy="${at(middle)}" r="6" fill="${colour(community.id)}"/>`
        + `<text x="${at(edge + 14)}" y="${at(middle + FONT_SIZE / 3)}">${escape(communityLabel(graph, community))}</text>`;
    });

    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${WIDTH * scale}" height="${height * scale}" viewBox="0 0 ${WIDTH} ${height}"`
      + ` font-family="Georgia, 'Times New Roman', serif">`
      + `<rect width="${WIDTH}" height="${height}" fill="${GROUND}"/>`
      + `<g stroke="${INK}" stroke-opacity="0.25">${lines.join('')}</g>`
      + `<g stroke="${RIM}" stroke-width="1.5">${circles.join('')}</g>`
      + `<g font-size="${FONT_SIZE}" fill="${INK}" stroke="${GROUND}" stroke-width="3" paint-order="stroke">${labels.join('')}${legend.join('')}</g>`
      + '</svg>';
    return { svg, width: WIDTH, height };
  }

  return { colour, community, communityLabel, image, mostPassages, radius };
})();

if (typeof module === 'object') module.exports = graphDraw;
