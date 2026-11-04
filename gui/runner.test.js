'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { test } = require('node:test');
const { buildArgs, findBinary, run, shown } = require('./runner');

const BOOK = path.join(__dirname, '..', 'tests', 'fixtures', 'alice.epub');

test('a tool that takes only a path gets just the path', () => {
  assert.deepEqual(buildArgs({ tool: 'info', path: '/b.pdf' }), ['info', '--', '/b.pdf']);
  assert.deepEqual(buildArgs({ tool: 'outline', path: '/b.pdf', query: 'ignored' }), ['outline', '--', '/b.pdf']);
  assert.deepEqual(buildArgs({ tool: 'graph', path: '/b.epub' }), ['graph', '--', '/b.epub']);
});

test('a query that starts with a dash is not read as a flag', () => {
  assert.deepEqual(
    buildArgs({ tool: 'search', path: '/b.pdf', query: '  -x marks the spot ' }),
    ['search', '--', '/b.pdf', '-x marks the spot'],
  );
  assert.deepEqual(buildArgs({ tool: 'find', path: '/b.pdf', query: 'the Queen' }), ['find', '--', '/b.pdf', 'the Queen']);
  assert.deepEqual(buildArgs({ tool: 'verify', path: '/b.pdf', query: 'a quote' }), ['verify', '--', '/b.pdf', 'a quote']);
});

test('read addresses a passage, a page or an outline entry', () => {
  const base = { tool: 'read', path: '/b.pdf', query: '12' };
  assert.deepEqual(buildArgs({ ...base, address: 'passage' }), ['read', '--', '/b.pdf', '12']);
  assert.deepEqual(buildArgs({ ...base, address: 'page' }), ['read', '--page', '12', '--', '/b.pdf']);
  assert.deepEqual(buildArgs({ ...base, address: 'outline' }), ['read', '--outline', '12', '--', '/b.pdf']);
});

test('a leading ~ in the path is the home directory', () => {
  assert.deepEqual(
    buildArgs({ tool: 'info', path: '~/b.epub' }),
    ['info', '--', path.join(os.homedir(), 'b.epub')],
  );
});

test('a form that cannot run says why and what to do', async () => {
  const cases = [
    [{ tool: 'nope', path: '/b.pdf' }, /unknown tool "nope"; choose one of info/],
    [{ tool: '__proto__', path: '/b.pdf' }, /unknown tool/],
    [{ tool: 'info', path: '  ' }, /path is empty; type the path.*Browse/],
    [{ tool: 'search', path: '/b.pdf', query: ' ' }, /query is empty/],
    [{ tool: 'read', path: '/b.pdf', query: 'twelve', address: 'page' }, /a page is a whole number/],
    [{ tool: 'read', path: '/b.pdf', query: '1', address: 'chapter' }, /unknown address "chapter"/],
  ];
  for (const [request, message] of cases) {
    const result = await run(request, '/does/not/matter');
    assert.equal(result.ok, false);
    assert.match(result.stderr, message);
  }
});

test('a missing binary names the path and the remedy', async () => {
  const result = await run({ tool: 'info', path: '/b.pdf' }, '/no/such/ebook-hero');
  assert.equal(result.ok, false);
  assert.match(result.stderr, /no ebook-hero binary at \/no\/such\/ebook-hero.*cargo build --release.*EBOOK_HERO_BIN/);
});

test('arguments reach the binary one by one, without a shell', { skip: process.platform === 'win32' }, async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'ebook-hero-gui-'));
  try {
    const fake = path.join(dir, 'fake');
    fs.writeFileSync(fake, '#!/bin/sh\nfor a in "$@"; do printf "[%s]" "$a"; done\necho "warning" >&2\n', { mode: 0o755 });
    const result = await run({ tool: 'search', path: '/my books/b.pdf', query: '$(touch pwned); -x' }, fake);
    assert.equal(result.ok, true);
    assert.equal(result.stdout, '[search][--][/my books/b.pdf][$(touch pwned); -x]');
    assert.equal(result.stderr, 'warning\n');
    assert.equal(fs.existsSync('pwned'), false);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('a failing command returns its own message and stdout', { skip: process.platform === 'win32' }, async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'ebook-hero-gui-'));
  try {
    const fake = path.join(dir, 'fake');
    fs.writeFileSync(fake, '#!/bin/sh\necho "error: no such book" >&2\nexit 1\n', { mode: 0o755 });
    const result = await run({ tool: 'info', path: '/b.pdf' }, fake);
    assert.deepEqual(result, { ok: false, stdout: '', stderr: 'error: no such book\n', binary: shown(fake) });
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

function executable(file) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, '#!/bin/sh\n', { mode: 0o755 });
  return file;
}

test('the binary comes from EBOOK_HERO_BIN, then the repository, then the PATH', { skip: process.platform === 'win32' }, () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ebook-hero-gui-'));
  try {
    const repository = path.join(root, 'repo');
    const env = { PATH: `${path.join(root, 'bin')}${path.delimiter}/no/such/dir` };
    const onPath = executable(path.join(root, 'bin', 'ebook-hero'));
    assert.equal(findBinary(env, repository), onPath);
    const built = executable(path.join(repository, 'target', 'release', 'ebook-hero'));
    assert.equal(findBinary(env, repository), built);
    assert.equal(findBinary({ ...env, EBOOK_HERO_BIN: '/elsewhere/ebook-hero' }, repository), '/elsewhere/ebook-hero');
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test('a file that is not executable, or no file at all, is not a binary', { skip: process.platform === 'win32' }, () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ebook-hero-gui-'));
  try {
    fs.mkdirSync(path.join(root, 'bin'));
    fs.writeFileSync(path.join(root, 'bin', 'ebook-hero'), 'not a program', { mode: 0o644 });
    fs.mkdirSync(path.join(root, 'dir', 'ebook-hero'), { recursive: true });
    const env = { PATH: [path.join(root, 'bin'), path.join(root, 'dir')].join(path.delimiter) };
    assert.equal(findBinary(env, path.join(root, 'repo')), null);
    assert.equal(findBinary({}, path.join(root, 'repo')), null);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test('with no binary at all, run says where it looked and what to do', async () => {
  const result = await run({ tool: 'info', path: '/b.pdf' }, null);
  assert.equal(result.ok, false);
  assert.match(result.stderr, /no ebook-hero binary found.*target\/release.*PATH.*cargo build --release.*cargo install.*EBOOK_HERO_BIN/);
});

test('a path for display writes the home directory as ~', () => {
  assert.equal(shown(path.join(os.homedir(), 'x', 'ebook-hero')), '~/x/ebook-hero');
  assert.equal(shown('/usr/bin/ebook-hero'), '/usr/bin/ebook-hero');
});

// These two need a binary (`cargo build --release`, or one on the PATH); neither needs the models.
const built = findBinary() !== null;

test('the real binary reports on a book', { skip: !built }, async () => {
  const result = await run({ tool: 'info', path: BOOK });
  assert.equal(result.ok, true, result.stderr);
  assert.match(result.stdout, /Alice/);
});

test('the real binary finds a phrase and reads the passage it names', { skip: !built }, async () => {
  const found = await run({ tool: 'find', path: BOOK, query: 'Cheshire Cat' });
  assert.equal(found.ok, true, found.stderr);
  const id = /passage (\d+)/.exec(found.stdout)?.[1];
  assert.ok(id, found.stdout);
  const read = await run({ tool: 'read', path: BOOK, query: id, address: 'passage' });
  assert.equal(read.ok, true, read.stderr);
  assert.match(read.stdout, /Cheshire/);
});
