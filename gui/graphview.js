'use strict';
// The Graph pane: draws the concept graph, lights a concept with its links and
// partners, and lists the passages of the concept clicked as links into the
// book. It knows nothing of commands or the form; renderer.js decides what to
// show and opens the passages.

const graphView = (() => {
  const SVG = 'http://www.w3.org/2000/svg';
  // The strip is drawn on this many units across, whatever its width.
  const STRIP_WIDTH = 1000;
  // Room around the drawing for the circles, and to their right for the labels.
  const MARGIN = { left: 20, top: 16, bottom: 16, rightShare: 0.2, rightMost: 140 };
  // One colour per community, in the window's palette; communities past these are grey.
  const COLOURS = ['#f0bf3c', '#43e6cb', '#e8798a', '#8fb8ff', '#b9e46a', '#d39bff', '#ff9f5a', '#7fd6f0'];
  const OTHER = '#8a93a6';

  const drawing = document.getElementById('graph-svg');
  const legend = document.getElementById('graph-legend');
  const title = document.getElementById('graph-concept');
  const strip = document.getElementById('graph-strip');
  const list = document.getElementById('graph-passages');
  let graph = null;
  let positions = [];
  let neighbours = [];

  const colour = (community) => COLOURS[community] ?? OTHER;

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
    const most = Math.max(1, ...graph.concepts.map((c) => c.passages.length));
    const concepts = element('g', { class: 'concepts' }, drawing);
    graph.concepts.forEach((concept, i) => {
      const r = 3 + 7 * Math.sqrt(concept.passages.length / most);
      const group = element('g', { class: 'concept', 'data-concept': i, tabindex: 0, role: 'button' }, concepts);
      element('circle', { cx: x(positions[i]), cy: y(positions[i]), r: r.toFixed(1), fill: colour(concept.community) }, group);
      const label = element('text', { x: x(positions[i]) + r + 4, y: y(positions[i]) + 4 }, group);
      label.textContent = concept.label;
      element('title', {}, group).textContent = `${concept.label}: ${concept.passages.length} passages`;
    });
  }

  function drawLegend() {
    legend.replaceChildren(...graph.communities.map((community) => {
      const top = community.concepts
        .map((id) => graph.concepts[id])
        .sort((p, q) => q.passages.length - p.passages.length)
        .slice(0, 4)
        .map((c) => c.label);
      const item = document.createElement('button');
      item.type = 'button';
      item.dataset.community = community.id;
      const swatch = document.createElement('span');
      swatch.className = 'swatch';
      swatch.style.background = colour(community.id);
      item.append(swatch, `${community.id + 1} · ${top.join(', ')}`);
      return item;
    }));
  }

  /** Lights concept `i`, its links and its partners, or the whole drawing again for null. */
  function light(i) {
    drawing.classList.toggle('focus', i !== null);
    const near = new Set(i === null ? [] : neighbours[i].map(([j]) => j));
    const links = new Set(i === null ? [] : neighbours[i].map(([, l]) => l));
    for (const node of drawing.querySelectorAll('.concept')) {
      const j = Number(node.dataset.concept);
      node.classList.toggle('on', j === i);
      node.classList.toggle('near', near.has(j));
    }
    for (const line of drawing.querySelectorAll('line')) line.classList.toggle('on', links.has(Number(line.dataset.link)));
  }

  /** Lights the concepts of community `c`, or the whole drawing again for null. */
  function lightCommunity(c) {
    drawing.classList.toggle('focus', c !== null);
    for (const node of drawing.querySelectorAll('.concept')) {
      node.classList.toggle('on', c !== null && graph.concepts[Number(node.dataset.concept)].community === c);
      node.classList.remove('near');
    }
    for (const line of drawing.querySelectorAll('line')) {
      const link = graph.links[Number(line.dataset.link)];
      line.classList.toggle('on', c !== null && graph.concepts[link.a].community === c && graph.concepts[link.b].community === c);
    }
  }

  /** Shows concept `i`'s passages on the strip and as links, or how to choose one for null. */
  function choose(i) {
    strip.replaceChildren();
    list.replaceChildren();
    if (i === null) {
      title.textContent = graph.concepts.length
        ? 'Click a concept to see its passages.'
        : 'No concept occurs in 3 or more passages of this book.';
      return;
    }
    const concept = graph.concepts[i];
    title.textContent = `${concept.label} · ${concept.passages.length} passages · community ${concept.community + 1}`;
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
  drawing.addEventListener('click', (event) => {
    const node = conceptOf(event);
    if (node) choose(Number(node.dataset.concept));
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
