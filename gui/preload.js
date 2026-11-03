'use strict';

const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('ebookHero', {
  tools: () => ipcRenderer.invoke('tools'),
  run: (request) => ipcRenderer.invoke('run', request),
  openBook: (request) => ipcRenderer.invoke('book', request),
  browse: () => ipcRenderer.invoke('browse'),
});
