"""A store of named blobs."""

import os

MAX_BLOBS = 1024
default_name = "blob"


class Error(Exception):
    pass


class Store:
    """A store of blobs, by name."""

    LIMIT = MAX_BLOBS

    def __init__(self, root):
        self.root = root
        self.blobs = {}

    @property
    def size(self):
        return len(self.blobs)

    def put(self, name, blob):
        def check(n):
            return n < self.LIMIT

        if not check(self.size):
            raise Error("full")
        self.blobs[name] = blob

    class Entry:
        def __repr__(self):
            return "Entry"


@staticmethod
def open_store(root):
    key = lambda name: name.lower()
    return Store(os.path.join(root, key("Blobs")))


async def fetch(store, name):
    return store.blobs.get(name)
