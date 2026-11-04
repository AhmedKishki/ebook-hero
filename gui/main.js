'use strict';

const path = require('node:path');
const { app, BrowserWindow, dialog, ipcMain } = require('electron');
const { createLibrary } = require('./book');
const { createGraphs } = require('./graph');
const { TOOLS, run } = require('./runner');

const library = createLibrary();
const graphs = createGraphs();

function createWindow() {
  const win = new BrowserWindow({
    width: 920,
    height: 720,
    minWidth: 560,
    minHeight: 420,
    icon: path.join(__dirname, 'icon.png'),
    autoHideMenuBar: true,
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
    },
  });
  // The window shows one local page and nothing from the network.
  win.webContents.setWindowOpenHandler(() => ({ action: 'deny' }));
  win.webContents.on('will-navigate', (event) => event.preventDefault());
  win.loadFile(path.join(__dirname, 'index.html'));
}

ipcMain.handle('tools', () => TOOLS);
ipcMain.handle('run', (_event, request) => run(request));
ipcMain.handle('book', (_event, request) => library.open(request));
ipcMain.handle('graph', (_event, request) => graphs.open(request));
ipcMain.handle('browse', async (event) => {
  const result = await dialog.showOpenDialog(BrowserWindow.fromWebContents(event.sender), {
    properties: ['openFile'],
    filters: [{ name: 'Books', extensions: ['pdf', 'epub'] }],
  });
  return result.canceled ? null : result.filePaths[0];
});

app.whenReady().then(createWindow);
app.on('window-all-closed', () => app.quit());
