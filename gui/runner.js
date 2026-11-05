'use strict';
// Turns the window's form into one `ebook-hero` command and runs it. It has no
// Electron in it, so `node --test` can exercise it without a display.

const { execFile } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

/**
 * The tools the window offers. `input` is the label of the text box the tool
 * needs, or null when the tool takes only a path. `addresses` is how `read`
 * names what to read; its text box then holds a number.
 */
const TOOLS = {
  info: { input: null },
  outline: { input: null },
  search: { input: 'Query' },
  find: { input: 'Phrase' },
  read: { input: 'Number', addresses: ['passage', 'page', 'outline'] },
  verify: { input: 'Quote' },
  graph: { input: null },
};

const REPOSITORY = path.join(__dirname, '..');

// A page of `find` or `read` output is far below this; the cap exists so that a
// runaway result is reported instead of silently cut.
const MAX_OUTPUT_BYTES = 256 * 1024 * 1024;
// The exit status of `ebook-hero verify` when the quote is not in the book.
const VERIFY_NONE = 3;

class FormError extends Error {}

/**
 * The binary the window runs, or null when there is none. The first of these
 * that exists wins: $EBOOK_HERO_BIN, `target/release/ebook-hero` in this
 * repository, then `ebook-hero` on the PATH. Every result names the binary it
 * ran, so a stale copy cannot hide.
 */
function findBinary(env = process.env, repository = REPOSITORY) {
  if (env.EBOOK_HERO_BIN) return env.EBOOK_HERO_BIN;
  const built = path.join(repository, 'target', 'release', 'ebook-hero');
  if (isExecutable(built)) return built;
  for (const dir of (env.PATH ?? '').split(path.delimiter).filter(Boolean)) {
    const candidate = path.join(dir, 'ebook-hero');
    if (isExecutable(candidate)) return candidate;
  }
  return null;
}

function isExecutable(file) {
  try {
    fs.accessSync(file, fs.constants.X_OK);
    return fs.statSync(file).isFile();
  } catch {
    return false;
  }
}

/** The arguments for one command. Throws FormError for a form that cannot run. */
function buildArgs({ tool, path: bookPath, query, address } = {}) {
  if (typeof tool !== 'string' || !Object.hasOwn(TOOLS, tool)) {
    throw new FormError(`unknown tool "${tool}"; choose one of ${Object.keys(TOOLS).join(', ')}`);
  }
  const spec = TOOLS[tool];
  const book = expandHome(String(bookPath ?? '').trim());
  if (!book) {
    throw new FormError('the path is empty; type the path of a PDF or EPUB, or press Browse');
  }
  if (!spec.input) {
    return [tool, '--', book];
  }
  const text = String(query ?? '').trim();
  if (!text) {
    throw new FormError(`the ${spec.input.toLowerCase()} is empty; type it in the box next to Run`);
  }
  if (!spec.addresses) {
    // `--` keeps a query that starts with a dash from being read as a flag.
    return [tool, '--', book, text];
  }
  const kind = address ?? 'passage';
  if (!spec.addresses.includes(kind)) {
    throw new FormError(`unknown address "${kind}"; choose one of ${spec.addresses.join(', ')}`);
  }
  if (!/^\d+$/.test(text)) {
    throw new FormError(`a ${kind} is a whole number, such as 12; "${text}" is not`);
  }
  return kind === 'passage'
    ? [tool, '--', book, text]
    : [tool, `--${kind}`, text, '--', book];
}

function expandHome(file) {
  return file === '~' || file.startsWith('~/') ? path.join(os.homedir(), file.slice(1)) : file;
}

/** A path for display: the home directory is `~`, so a screenshot carries no user name. */
function shown(file) {
  const home = os.homedir();
  return file === home || file.startsWith(home + path.sep) ? `~${file.slice(home.length)}` : file;
}

/**
 * Runs the form's command. The result is the one `execBinary` gives; a form
 * that cannot run is a result with `ok: false` and a message.
 */
function run(request, binary = findBinary()) {
  let args;
  try {
    args = buildArgs(request);
  } catch (error) {
    if (!(error instanceof FormError)) throw error;
    return Promise.resolve(failure(`error: ${error.message}`));
  }
  return execBinary(args, binary);
}

/**
 * Runs the binary with `args`. Resolves to `{ ok, stdout, stderr, binary }`,
 * where `binary` is the displayable path of the binary that ran; a failure of
 * any kind is a result with `ok: false` and a message, never a rejection.
 */
function execBinary(args, binary = findBinary()) {
  return new Promise((resolve) => {
    if (!binary) {
      resolve(failure(
        'error: no ebook-hero binary found, neither in this repository (target/release) nor on the PATH; '
          + 'run "cargo build --release" in the repository root, install it with '
          + '"cargo install --locked --git https://github.com/AhmedKishki/ebook-hero", '
          + 'or set EBOOK_HERO_BIN to the binary',
      ));
      return;
    }
    execFile(
      binary,
      args,
      { encoding: 'utf8', maxBuffer: MAX_OUTPUT_BYTES },
      (error, stdout, stderr) => {
        const ran = shown(binary);
        // verify exits with 3 when the quote is not in the book: a verdict, not a failure.
        const verdictNone = error?.code === VERIFY_NONE && args[0] === 'verify';
        if (!error || verdictNone) {
          resolve({ ok: true, stdout, stderr, binary: ran });
        } else if (error.code === 'ENOENT') {
          resolve(failure(
            `error: no ebook-hero binary at ${ran}; run "cargo build --release" in the repository root, or set EBOOK_HERO_BIN to the binary`,
          ));
        } else if (error.code === 'ERR_CHILD_PROCESS_STDIO_MAXBUFFER') {
          resolve(failure(
            `error: the output is larger than ${MAX_OUTPUT_BYTES} bytes; use the command line with a narrower request`,
          ));
        } else {
          // The command failed: its own message on stderr names the cause.
          resolve({ ok: false, stdout, stderr: stderr || `error: ${error.message}`, binary: ran });
        }
      },
    );
  });
}

function failure(message) {
  return { ok: false, stdout: '', stderr: message };
}

/**
 * The book file `file` names: `{ ok: true, absolute, key }`, where `key`
 * changes whenever the file does, or a failure that says what is wrong with
 * the path.
 */
function locateBook(file) {
  const given = String(file ?? '').trim();
  if (!given) {
    return failure('error: the path is empty; type the path of a PDF or EPUB, or press Browse');
  }
  let absolute;
  let stat;
  try {
    absolute = path.resolve(expandHome(given));
    stat = fs.statSync(absolute);
  } catch (error) {
    return failure(`error: cannot read ${given} (${error.code ?? error.message}); check the path`);
  }
  if (!stat.isFile()) {
    return failure(`error: ${given} is not a file; type the path of a PDF or EPUB`);
  }
  return { ok: true, absolute, key: `${absolute}|${stat.size}|${stat.mtimeMs}` };
}

module.exports = { TOOLS, buildArgs, execBinary, expandHome, failure, findBinary, locateBook, run, shown };
