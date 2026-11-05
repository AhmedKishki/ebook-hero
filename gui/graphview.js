'use strict';
// The Graph pane: draws the concept graph, lights a concept with its links and
// partners while it is pointed at, keeps the community of the concept clicked
// lit, and lists that concept's passages as links into the book. It knows
// nothing of commands or the form; renderer.js decides what to show and opens
// the passages. graphdraw.js holds the colours and names it shares with the
// saved image.

const graphView = (() => {
  const SVG = 'http://www.w3.org/2000/svg';
  // The strip is drawn on this many units across, whatever its width.
  const STRIP_WIDTH = 1000;
  // Room around the drawing for the circles, and to their right for the labels.
  const MARGIN = { left: 20, top: 16, bottom: 16, rightShare: 0.2, rightMost: 140 };

  const drawing = document.getElementById('graph-svg');
  const legend = document.getElementById('graph-legend');
  const title = document.getElementById('graph-concept');
  const note = document.getElementById('graph-note');
  const strip = document.getElementById('graph-strip');
  const list = document.getElementById('graph-passages');
  let graph = null;
  let positions = [];
  let neighbours = [];
  // The concept clicked, whose community stays lit, or null.
  let chosen = null;

  function element(name, attributes = {}, parent = null) {
    const node = document.createElementNS(SVG, name);
    for (const [key, value] of Object.entries(attributes)) node.setAttribute(key, value);
    parent?.append(node);
    return node;
  }

  /** Shows `shown` with each concept at its place in `placed` (each in [0, 1]²). */
  function show(shown, placed) {
    graph = shown;
    positions = placed;
    chosen = null;
    neighbours = graph.concepts.map(() => []);
    graph.links.forEach((link, i) => {
      neighbours[link.a].push([link.b, i]);
      neighbours[link.b].push([link.a, i]);
    });
    draw();
    drawLegend();
    choose(null);
  }

  /**
   * Draws the graph at the drawing's own size in pixels, so that its labels
   * stay readable however small the window is.
   */
  function draw() {
    const width = Math.max(drawing.clientWidth, 300);
    const height = Math.max(drawing.clientHeight, 200);
    const right = Math.min(MARGIN.rightMost, width * MARGIN.rightShare);
    const x = (p) => MARGIN.left + p.x * (width - MARGIN.left - right);
    const y = (p) => MARGIN.top + p.y * (height - MARGIN.top - MARGIN.bottom);
    drawing.setAttribute('viewBox', `0 0 ${width} ${height}`);
    drawing.replaceChildren();
    const links = element('g', { class: 'links' }, drawing);
    graph.links.forEach((link, i) => {
      const [a, b] = [positions[link.a], positions[link.b]];
      element('line', {
        x1: x(a), y1: y(a), x2: x(b), y2: y(b),
        'stroke-width': (0.8 + 2.4 * link.npmi).toFixed(2),
        'data-link': i,
      }, links);
    });
    const most = graphDraw.mostPassages(graph);
    const concepts = element('g', { class: 'concepts' }, drawing);
    graph.concepts.forEach((concept, i) => {
      const r = graphDraw.radius(concept, most);
      const group = element('g', { class: 'concept', 'data-concept': i, tabindex: 0, role: 'button' }, concepts);
      element('circle', { cx: x(positions[i]), cy: y(positions[i]), r: r.toFixed(1), fill: graphDraw.colour(concept.community) }, group);
      const label = element('text', { x: x(positions[i]) + r + 4, y: y(positions[i]) + 4 }, group);
      label.textContent = concept.label;
      element('title', {}, group).textContent = `${concept.label}: ${concept.passages.length} passages`;
    });
    rest();
  }

  function drawLegend() {
    legend.replaceChildren(...graph.communities.map((community) => {
      const item = document.createElement('button');
      item.type = 'button';
      item.dataset.community = community.id;
      const swatch = document.createElement('span');
      swatch.className = 'swatch';
      swatch.style.background = graphDraw.colour(community.id);
      item.append(swatch, graphDraw.communityLabel(graph, community));
      return item;
    }));
  }

  /**
   * Lights the concepts in `on`, half-lights those in `near` and lights the
   * links in `links`, dimming the rest; for null, lights the whole drawing.
   * The chosen concept keeps its ring either way.
   */
  function paint(on, near = new Set(), links = new Set()) {
    drawing.classList.toggle('focus', on !== null);
    for (const node of drawing.querySelectorAll('.concept')) {
      const j = Number(node.dataset.concept);
      node.classList.toggle('on', on?.has(j) ?? false);
      node.classList.toggle('near', near.has(j));
      node.classList.toggle('chosen', j === chosen);
    }
    for (const line of drawing.querySelectorAll('line')) line.classList.toggle('on', links.has(Number(line.dataset.link)));
  }

  /** Lights concept `i`, its links and its partners while it is pointed at; null goes back to the chosen concept. */
  function light(i) {
    if (i === null) return rest();
    paint(new Set([i]), new Set(neighbours[i].map(([j]) => j)), new Set(neighbours[i].map(([, l]) => l)));
  }

  /** Lights the concepts of community `c` and the links between them; null goes back to the chosen concept. */
  function lightCommunity(c) {
    if (c === null) return rest();
    const lit = graphDraw.community(graph, c);
    paint(lit.concepts, new Set(), lit.links);
  }

  /** Lights the chosen concept's community, or the whole drawing when no concept is chosen. */
  function rest() {
    if (chosen === null) paint(null);
    else lightCommunity(graph.concepts[chosen].community);
  }

  /**
   * Chooses concept `i`: rings it, keeps its community lit, and shows its
   * passages on the strip and as links. For null, lights the whole drawing
   * and says how to choose one.
   */
  function choose(i) {
    chosen = i;
    rest();
    strip.replaceChildren();
    list.replaceChildren();
    if (i === null) {
      title.textContent = graph.concepts.length
        ? 'Click a concept to see its passages.'
        : 'No phrase of this book occurs in at least 3 passages and in at most 30% of them, so it has no concepts.';
      note.textContent = '';
      return;
    }
    const concept = graph.concepts[i];
    title.textContent = `${concept.label} · ${concept.passage_count} passages · community ${concept.community + 1}`;
    note.textContent = `The passages where “${concept.label}” stands as a phrase of its own. find lists every mention.`;
    strip.setAttribute('viewBox', `0 0 ${STRIP_WIDTH} 30`);
    strip.setAttribute('preserveAspectRatio', 'none');
    element('line', { x1: 0, x2: STRIP_WIDTH, y1: 15, y2: 15, class: 'axis' }, strip);
    for (const p of concept.passages) {
      const at = (p / Math.max(1, graph.passages - 1)) * (STRIP_WIDTH - 2);
      element('rect', { x: at.toFixed(1), y: 4, width: 2, height: 22 }, strip);
    }
    list.replaceChildren(...concept.passages.map((p) => {
      const item = document.createElement('li');
      const link = document.createElement('a');
      link.href = '#';
      link.dataset.passage = p;
      link.title = 'Show in the book';
      link.textContent = `passage ${p}`;
      item.append(link);
      return item;
    }));
  }

  const conceptOf = (event) => event.target.closest?.('.concept');
  drawing.addEventListener('pointerover', (event) => {
    const node = conceptOf(event);
    if (node) light(Number(node.dataset.concept));
  });
  drawing.addEventListener('pointerout', (event) => {
    if (conceptOf(event) && !conceptOf({ target: event.relatedTarget ?? drawing })) light(null);
  });
  // A click on a concept chooses it, and a click on the empty drawing lets it go.
  drawing.addEventListener('click', (event) => {
    const node = conceptOf(event);
    choose(node ? Number(node.dataset.concept) : null);
  });
  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape' && graph && chosen !== null && drawing.clientWidth > 0) choose(null);
  });
  drawing.addEventListener('keydown', (event) => {
    const node = conceptOf(event);
    if (node && (event.key === 'Enter' || event.key === ' ')) {
      event.preventDefault();
      choose(Number(node.dataset.concept));
    }
  });
  drawing.addEventListener('focusin', (event) => {
    const node = conceptOf(event);
    if (node) light(Number(node.dataset.concept));
  });
  drawing.addEventListener('focusout', () => light(null));
  legend.addEventListener('pointerover', (event) => {
    const item = event.target.closest?.('button');
    if (item) lightCommunity(Number(item.dataset.community));
  });
  legend.addEventListener('pointerleave', () => lightCommunity(null));
  // A resized window, or the pane coming into view, redraws at the new size.
  new ResizeObserver(() => {
    if (graph && drawing.clientWidth > 0) draw();
  }).observe(drawing);

  return { show };
})();
