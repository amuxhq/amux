#include <stdlib.h>
#include <string.h>

#define MAX_BLOBS 1024

static const size_t max_name = 64;

/* A blob and its name. */
struct blob {
    char *name;
    size_t len;
};

typedef struct {
    struct blob *blobs;
    size_t count;
} store;

enum error {
    ERROR_NONE,
    ERROR_FULL,
};

union value {
    int number;
    char *text;
};

static int full(const store *s) {
    return s->count >= MAX_BLOBS;
}

/* Makes an empty store. */
store *store_open(void) {
    store *s = malloc(sizeof(store));
    memset(s, 0, sizeof(store));
    return s;
}

enum error store_put(store *s, struct blob b) {
    if (full(s)) {
        return ERROR_FULL;
    }
    s->blobs[s->count++] = b;
    return ERROR_NONE;
}
