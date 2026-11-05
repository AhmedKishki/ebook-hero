'use strict';
// The model behind the window's Help view: the usage guide, which is
// docs/usage.md in the repository the window runs from, the help the binary
// prints for the tool the form has chosen, and the links the guide may open in
// the browser. It has no Electron in it, so `node --test` can exercise it
// without a display.

const fs = require('node:fs');
const path = require('node:path');
const { TOOLS, execBinary, failure, shown } = require('./runner');

const GUIDE = path.join(__dirname, '..', 'docs', 'usage.md');
// A link in the guide to another file of docs/ opens that file here.
const DOCS_URL = 'https://github.com/AhmedKishki/ebook-hero/blob/main/docs/';

/**
 * The usage guide: `{ ok: true, text, base }`, where `base` is the address
 * its relative links resolve against, or a failure that names the file.
 */
async function readGuide(file = GUIDE) {
  try {
    return { ok: true, text: await fs.promises.readFile(file, 'utf8'), base: DOCS_URL };
  } catch (error) {
    return failure(
      `error: cannot read the usage guide ${shown(file)} (${error.code ?? error.message}); `
        + 'restore it with "git checkout -- docs/usage.md" in the repository root, or run the window from a clone of ebook-hero',
    );
  }
}

/**
 * What `ebook-hero help <tool>` prints, through `exec`, which tests replace
 * with a fake. Resolves to what `execBinary` gives, or to a failure for a
 * tool the form does not offer, which runs nothing.
 */
function commandHelp(tool, exec = execBinary) {
  if (typeof tool !== 'string' || !Object.hasOwn(TOOLS, tool)) {
    return Promise.resolve(failure(`error: unknown tool "${tool}"; choose one of ${Object.keys(TOOLS).join(', ')}`));
  }
  return exec(['help', tool]);
}

/**
 * Opens `url` in the system browser through `open`, which is Electron's
 * `shell.openExternal`. Only an http or https address is opened, so a link
 * cannot start another program through a scheme such as `file:`. Resolves to
 * `{ ok: true }` or to a failure.
 */
async function openLink(url, open) {
  let address = null;
  try {
    address = new URL(String(url));
  } catch {
    // Not an address at all; refused below.
  }
  if (!address || !['http:', 'https:'].includes(address.protocol)) {
    return failure(
      `error: the window opens only http and https links in the browser, and ${JSON.stringify(String(url))} is neither; `
        + 'give the link in docs/usage.md an http or https address',
    );
  }
  try {
    await open(address.href);
  } catch (error) {
    return failure(`error: could not open ${address.href} in the browser (${error.message}); paste the address into a browser`);
  }
  return { ok: true };
}

module.exports = { commandHelp, openLink, readGuide };
