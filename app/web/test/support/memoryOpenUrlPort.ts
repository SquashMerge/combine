import type { OpenUrlPort } from '../../src/ipc/port';

export class MemoryOpenUrlPort implements OpenUrlPort {
  readonly urls: string[] = [];

  openUrl(url: string): Promise<void> {
    this.urls.push(url);
    return Promise.resolve();
  }
}
