'use strict';
// The Help pane: the help the binary prints for the tool the form has chosen,
// and the usage guide with its contents beside it. markdown.js turns the guide
// into HTML; this file puts it on the page and follows links to its headings
// within the guide. It knows nothing of commands or the form; renderer.js
// decides what to show and opens links to other sites.

const helpView = (() => {
  // Before every heading id of the guide, so none can clash with an id of the window's own, such as `path`.
  const ID_PREFIX = 'help-';
  // Room left above a heading the guide scrolls to, in pixels.
  const ABOVE = 8;

  const title = document.getElementById('help-command-title');
  const commandText = document.getElementById('help-command-text');
  const contents = document.getElementById('help-contents');
  const guide = document.getElementById('help-guide');
  // The Markdown of the guide on show, so an unchanged guide keeps its place.
  let shownText = null;

  /** Shows the guide written in `text`, whose relative links resolve against `base`. */
  function showGuide(text, base) {
    if (text === shownText) return;
    const page = markdown.render(text, { base, idPrefix: ID_PREFIX });
    // markdown.js escapes every piece of text, and links only to the guide's headings and to http(s) addresses.
    guide.innerHTML = page.html;
    guide.scrollTop = 0;
    contents.replaceChildren(list(markdown.contents(page.headings)));
    shownText = text;
  }

  function list(entries) {
    const items = document.createElement('ol');
    for (const entry of entries) {
      const item = document.createElement('li');
      const link = document.createElement('a');
      link.href = `#${entry.id}`;
      link.textContent = entry.text;
      item.append(link);
      if (entry.children.length > 0) item.append(list(entry.children));
      items.append(item);
    }
    return items;
  }

  /** Shows `message` where the guide would be, and no contents. */
  function showGuideFailure(message) {
    const note = document.createElement('p');
    note.className = 'failed';
    note.textContent = message;
    guide.replaceChildren(note);
    contents.replaceChildren();
    shownText = null;
  }

  /** Shows what `ebook-hero help <tool>` gave: `reply` is `{ ok, stdout, stderr }`. */
  function showCommand(tool, reply) {
    title.textContent = `ebook-hero help ${tool}`;
    commandText.textContent = reply.ok ? reply.stdout : reply.stderr;
    commandText.classList.toggle('failed', !reply.ok);
    commandText.scrollTop = 0;
  }

  // A link to a heading, in the contents or in the guide, scrolls the guide alone to that heading.
  for (const pane of [contents, guide]) {
    pane.addEventListener('click', (event) => {
      const link = event.target.closest('a[href^="#"]');
      if (!link) return;
      event.preventDefault();
      const heading = document.getElementById(link.getAttribute('href').slice(1));
      if (!heading || !guide.contains(heading)) return;
      const top = heading.getBoundingClientRect().top - guide.getBoundingClientRect().top;
      guide.scrollTo({ top: guide.scrollTop + top - ABOVE });
    });
  }

  return { showCommand, showGuide, showGuideFailure };
})();
