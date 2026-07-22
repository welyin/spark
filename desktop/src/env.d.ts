import type { ElectronAPI } from './api';

declare global {
  interface Window {
    electronAPI: ElectronAPI;
  }
}

export {};
