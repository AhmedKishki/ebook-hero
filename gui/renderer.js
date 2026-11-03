'use strict';

const $ = (id) => document.getElementById(id);
const tool = $('tool');
const bookPath = $('path');
const query = $('query');
const address = $('address');
const runButton = $('run');
const status = $('status');
const out = $('out');
const err = $('err');
const bookText = $('book-text');
const tabResult = $('tab-result');
const tabBook = $('tab-book');

// "passage 12" where the tools print a passage: before " ·" in a hit, a read
// result or a verify result, and before "," in an outline entry.
const PASSAGE = /passage (\d+)(?= ·|,)/g;

let tools = {};
// The book of the last run, as typed then: a passage link means that book, even
// if the box has been edited since.
let lastRun = { path: '' };
// The book on show in the Book pane, the key main gave it, and what the line said of it.
let open = { key: null, title: null, note: '' };
let opening = false;

// The one line beside the tabs says something different for each pane.
const lines = { result: { text: '', failed: false }, book: { text: '', failed: false } };
let pane = 'result';

function say(which, text, failed = false) {
  lines[which] = { text, failed };
  if (which === pane) paintLine();
}

function paintLine() {
  status.textContent = lines[pane].text;
  status.title = lines[pane].text;
  status.classList.toggle('failed', lines[pane].failed);
}

/** Shows the box the chosen tool needs, named in its placeholder, and disables it when none is. */
function sync() {
  const spec = tools[tool.value];
  const needsInput = spec.input !== null;
  query.disabled = !needsInput;
  query.placeholder = !needsInput ? 'Not needed for this tool' : spec.addresses ? 'Number, e.g. 12' : spec.input;
  query.setAttribute('aria-label', spec.input ?? 'Not needed for this tool');
  address.hidden = !spec.addresses;
  if (!needsInput) query.value = '';
}

function showPane(name) {
  pane = name;
  const book = name === 'book';
  out.hidden = book;
  err.hidden = book;
  bookText.hidden = !book;
  tabResult.setAttribute('aria-selected', String(!book));
  tabBook.setAttribute('aria-selected', String(book));
  paintLine();
}

/** Shows a command's output, with each passage it names as a link into the book. */
function showResult(text) {
  const fragment = document.createDocumentFragment();
  let from = 0;
  for (const match of text.matchAll(PASSAGE)) {
    if (match.index > from) fragment.append(text.slice(from, match.index));
    const link = document.createElement('a');
    link.href = '#';
    link.dataset.passage = match[1];
    link.title = 'Show in the book';
    link.textContent = match[0];
    fragment.append(link);
    from = match.index + match[0].length;
  }
  if (from < text.length) fragment.append(text.slice(from));
  out.replaceChildren(fragment);
}

async function run(event) {
  event.preventDefault();
  runButton.disabled = true;
  showResult('');
  err.textContent = '';
  err.classList.remove('failed');
  say('result', 'Running… the first search of a book builds its index, which can take a while.');
  showPane('result');
  lastRun = { path: bookPath.value };
  let result;
  try {
    result = await window.ebookHero.run({
      tool: tool.value,
      path: bookPath.value,
      query: query.value,
      address: address.value,
    });
  } catch (error) {
    result = { ok: false, stdout: '', stderr: `error: ${error.message}` };
  }
  showResult(result.stdout);
  err.textContent = result.stderr;
  err.classList.toggle('failed', !result.ok);
  const ran = result.binary ? ` Ran ${result.binary}.` : '';
  say('result', `${result.ok ? 'Done.' : 'Failed.'}${ran}`, !result.ok);
  runButton.disabled = false;
}

/**
 * Opens the Book pane on the book at `path`, and with `passage`, marks that
 * passage and scrolls to it.
 */
async function showBook(path, passage) {
  if (opening) return;
  opening = true;
  say('book', 'Opening the book… a large one can take a while the first time.');
  showPane('book');
  let reply;
  try {
    reply = await window.ebookHero.openBook({ path, passage, have: open.key });
  } catch (error) {
    reply = { ok: false, stderr: `error: ${error.message}` };
  }
  opening = false;
  if (!reply.ok) {
    say('book', reply.stderr.trim(), true);
    return;
  }
  if (reply.view) {
    viewer.show(reply.view);
    open = { key: reply.key, title: reply.view.title ?? 'Untitled book', note: '' };
    open.note = open.title;
  }
  if (passage !== undefined) {
    const found = viewer.mark(reply.ids);
    open.note = reply.precise && found > 0
      ? `${open.title} · passage ${passage}`
      : `${open.title} · passage ${passage}: its page is marked, because its exact paragraphs could not be told apart`;
  }
  say('book', open.note);
}

async function browse() {
  const chosen = await window.ebookHero.browse();
  if (chosen) bookPath.value = chosen;
}

async function start() {
  tools = await window.ebookHero.tools();
  for (const name of Object.keys(tools)) tool.add(new Option(name, name));
  for (const kind of tools.read.addresses) address.add(new Option(kind, kind));
  tool.addEventListener('change', sync);
  $('form').addEventListener('submit', run);
  $('browse').addEventListener('click', browse);
  tabResult.addEventListener('click', () => showPane('result'));
  tabBook.addEventListener('click', () => showBook(bookPath.value));
  out.addEventListener('click', (event) => {
    const link = event.target.closest('a[data-passage]');
    if (!link) return;
    event.preventDefault();
    showBook(lastRun.path, Number(link.dataset.passage));
  });
  sync();
}

start();
