'use strict';
// The Book pane: draws the extracted book and marks the blocks of a passage.
// It knows nothing of commands or the form; renderer.js decides what to show.
// A reader's navigation (contents, position, bookmarks) would be added here.

const viewer = (() => {
  const text = document.getElementById('book-text');
  let elements = new Map(); // block id -> its element
  let marked = [];

  /** Draws every block of `view` (see viewOf in book.js), replacing the book on show. */
  function show(view) {
    elements = new Map();
    marked = [];
    const fragment = document.createDocumentFragment();
    for (const block of view.blocks) {
      const element = document.createElement(
        block.type === 'heading' ? `h${Math.min(Math.max(block.level ?? 1, 1), 6)}` : 'p',
      );
      element.textContent = block.text;
      if (block.type === 'list_item') element.className = 'item';
      elements.set(block.id, element);
      fragment.append(element);
    }
    text.replaceChildren(fragment);
    text.scrollTop = 0;
  }

  /** Marks the blocks with these ids, and scrolls to the first. Returns how many it found. */
  function mark(ids) {
    for (const element of marked) element.classList.remove('mark');
    marked = ids.map((id) => elements.get(id)).filter(Boolean);
    for (const element of marked) element.classList.add('mark');
    marked[0]?.scrollIntoView({ block: 'center' });
    return marked.length;
  }

  return { show, mark };
})();
