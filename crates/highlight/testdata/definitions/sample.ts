import { readFile } from "node:fs/promises";

export const MAX_BLOBS = 1024;

export interface Backend {
  read(name: string): Uint8Array | undefined;
}

export type Blobs = Map<string, Uint8Array>;

export enum Kind {
  Small,
  Large,
}

/** A store of blobs, by name. */
export class Store implements Backend {
  private blobs: Blobs = new Map();

  constructor(private readonly root: string) {}

  read(name: string): Uint8Array | undefined {
    return this.blobs.get(name);
  }

  put(name: string, blob: Uint8Array): void {
    const check = (n: number) => n < MAX_BLOBS;
    if (!check(this.blobs.size)) {
      throw new Error("full");
    }
    this.blobs.set(name, blob);
  }

  onChange = (): void => {};
}

export abstract class Base {
  abstract open(): void;
}

export namespace Stores {
  export function open(root: string): Store {
    return new Store(root);
  }
}

export const load = async (store: Store, path: string): Promise<void> => {
  store.put(path, await readFile(path));
};
