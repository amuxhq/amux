import { readFile } from "node:fs/promises";

export const MAX_BLOBS = 1024;

let opened = 0;

/** A store of blobs, by name. */
export class Store {
  static limit = MAX_BLOBS;

  constructor(root) {
    this.root = root;
    this.blobs = new Map();
  }

  put(name, blob) {
    const check = (n) => n < Store.limit;
    if (!check(this.blobs.size)) {
      throw new Error("full");
    }
    this.blobs.set(name, blob);
  }

  onChange = () => {
    opened += 1;
  };

  get size() {
    return this.blobs.size;
  }
}

export function openStore(root) {
  function helper() {
    return root;
  }
  return new Store(helper());
}

export const load = async (store, path) => {
  const data = await readFile(path);
  store.put(path, data);
};

function* names(store) {
  yield* store.blobs.keys();
}
