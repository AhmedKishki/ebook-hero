#!/usr/bin/env node
'use strict';
// Opens the window. `npm start` runs it, and `npm link` in this folder puts it on
// the PATH as `ebook-hero-gui`, so the window opens from any directory. Extra
// arguments, such as --no-sandbox, go to Electron.

const { spawn } = require('node:child_process');

let electron;
try {
  // In Node, this is the path of the Electron executable.
  electron = require('electron');
} catch (error) {
  console.error(`error: Electron is not installed (${error.message}); run "npm install" in ${__dirname}`);
  process.exit(1);
}

// A shell started by VS Code can carry ELECTRON_RUN_AS_NODE=1, which makes
// Electron run this folder as a plain Node script instead of opening the window.
const { ELECTRON_RUN_AS_NODE, ...env } = process.env;

const child = spawn(electron, [__dirname, ...process.argv.slice(2)], { stdio: 'inherit', env });
child.on('error', (error) => {
  console.error(`error: could not start Electron at ${electron}: ${error.message}; run "npm install" in ${__dirname}`);
  process.exit(1);
});
child.on('exit', (code) => process.exit(code ?? 1));
// Closing the terminal or stopping this process closes the window too.
for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
  process.on(signal, () => child.kill(signal));
}
