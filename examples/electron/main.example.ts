/**
 * Example Electron Main Process Integration
 */

import { app, BrowserWindow, ipcMain } from 'electron';
import * as path from 'path';
import { ThunderClient } from './thunder-client';

let mainWindow: BrowserWindow | null = null;
const thunder = new ThunderClient({
  useMock: process.env.NODE_ENV !== 'production' && !process.env.OPENAI_API_KEY,
});

function createWindow() {
  mainWindow = new BrowserWindow({
    width: 1200,
    height: 800,
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false,
    },
  });

  mainWindow.loadURL('http://localhost:3000'); // or mainWindow.loadFile(...)
}

app.whenReady().then(() => {
  // 1. Start Thunder Daemon Sidecar
  thunder.start();

  createWindow();

  // 2. Register IPC Handlers for Frontend
  ipcMain.handle('thunder:ping', async () => {
    return await thunder.ping();
  });

  ipcMain.handle('thunder:list-models', async () => {
    return await thunder.listModels();
  });

  ipcMain.handle('thunder:run-task', async (event, opts: { taskId: string; prompt: string; sessionId?: string; model?: string }) => {
    return await thunder.runTask({
      taskId: opts.taskId,
      prompt: opts.prompt,
      sessionId: opts.sessionId,
      model: opts.model,
      onEvent: (agentEvent) => {
        // Broadcast token deltas and tool execution updates to UI
        event.sender.send('thunder:stream-event', {
          taskId: opts.taskId,
          event: agentEvent,
        });
      },
    });
  });

  ipcMain.handle('thunder:cancel-task', async (_event, taskId: string) => {
    return await thunder.cancelTask(taskId);
  });
});

app.on('before-quit', () => {
  // Ensure child process is killed when Electron quits
  thunder.stop();
});
