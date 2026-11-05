'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { test } = require('node:test');
const { commandHelp, openLink, readGuide } = require('./help');
const { TOOLS, findBinary } = require('./runner');

test('the guide is docs/usage.md of this repository, with GitHub as the base of its relative links', async () => {
  const guide = await readGuide();
  assert.equal(guide.ok, true, guide.stderr);
  assert.equal(guide.text, fs.readFileSync(path.join(__dirname, '..', 'docs', 'usage.md'), 'utf8'));
  assert.equal(guide.base, 'https://github.com/AhmedKishki/ebook-hero/blob/main/docs/');
});

test('a guide that cannot be read names its path and how to restore it', async () => {
  const missing = '/no/such/docs/usage.md';
  const guide = await readGuide(missing);
  assert.equal(guide.ok, false);
  assert.ok(guide.stderr.includes(missing), guide.stderr);
  assert.match(guide.stderr, /ENOENT/);
  assert.match(guide.stderr, /git checkout -- docs\/usage\.md/);
});

test('the help of a tool is "help <tool>", for every tool the form offers', async () => {
  const calls = [];
  const exec = async (args) => {
    calls.push(args);
    return { ok: true, stdout: 'Usage: …', stderr: '', binary: '/bin/ebook-hero' };
  };
  for (const tool of Object.keys(TOOLS)) {
    assert.deepEqual(await commandHelp(tool, exec), { ok: true, stdout: 'Usage: …', stderr: '', binary: '/bin/ebook-hero' });
  }
  assert.deepEqual(calls, Object.keys(TOOLS).map((tool) => ['help', tool]));
});

test('the help of a tool the form does not offer runs nothing and says what to choose', async () => {
  const exec = async () => assert.fail('the binary must not run');
  for (const tool of ['cache', 'mcp', '--version', '__proto__', '', undefined, 3]) {
    const reply = await commandHelp(tool, exec);
    assert.equal(reply.ok, false);
    assert.match(reply.stderr, /unknown tool .*; choose one of info, outline, search/);
  }
});

test('a failing binary\'s own message comes back', async () => {
  const exec = async () => ({ ok: false, stdout: '', stderr: "error: unrecognized subcommand 'graph'\n", binary: '/old/ebook-hero' });
  const reply = await commandHelp('graph', exec);
  assert.deepEqual(reply, { ok: false, stdout: '', stderr: "error: unrecognized subcommand 'graph'\n", binary: '/old/ebook-hero' });
});

test('openLink opens http and https addresses and refuses every other', async () => {
  const opened = [];
  const open = async (url) => opened.push(url);
  assert.deepEqual(await openLink('https://docs.rs/ebook-hero', open), { ok: true });
  assert.deepEqual(await openLink('HTTP://example.com/a b', open), { ok: true });
  for (const url of ['javascript:alert(1)', 'file:///etc/passwd', 'search.md', '#cache', 'mailto:a@b.c', '', null]) {
    const reply = await openLink(url, open);
    assert.equal(reply.ok, false, String(url));
    assert.match(reply.stderr, /only http and https.*; give the link in docs\/usage\.md an http or https address/);
  }
  assert.deepEqual(opened, ['https://docs.rs/ebook-hero', 'http://example.com/a%20b']);
});

test('openLink says when the browser cannot be started, with the address to paste', async () => {
  const reply = await openLink('https://docs.rs/ebook-hero', async () => {
    throw new Error('no browser');
  });
  assert.equal(reply.ok, false);
  assert.match(reply.stderr, /could not open https:\/\/docs\.rs\/ebook-hero in the browser \(no browser\); paste the address/);
});

// This one needs a binary (`cargo build --release`, or one on the PATH).
test('the real binary prints the help of each tool the form offers', { skip: findBinary() === null }, async () => {
  for (const tool of Object.keys(TOOLS)) {
    const reply = await commandHelp(tool);
    assert.equal(reply.ok, true, reply.stderr);
    assert.match(reply.stdout, new RegExp(`Usage: ebook-hero ${tool} `));
  }
});
