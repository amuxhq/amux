package blobs;

import java.util.HashMap;
import java.util.Map;

/** A store of blobs, by name. */
public class Store {
    public static final int MAX_BLOBS = 1024;

    private final Map<String, byte[]> blobs = new HashMap<>();

    public Store() {
    }

    @Override
    public String toString() {
        return "Store";
    }

    public void put(String name, byte[] blob) throws StoreException {
        Runnable check = () -> {
            if (blobs.size() >= MAX_BLOBS) {
                throw new IllegalStateException();
            }
        };
        check.run();
        blobs.put(name, blob);
    }

    static class Entry {
        String name;

        String describe() {
            return name;
        }
    }

    enum Kind {
        SMALL,
        LARGE;

        boolean isLarge() {
            return this == LARGE;
        }
    }
}

interface Backend {
    byte[] read(String name);
}

record Blob(String name, byte[] data) {
    Blob {
        name = name.trim();
    }
}
