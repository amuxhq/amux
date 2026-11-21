package store

import "errors"

// MaxBlobs is the most blobs a store holds.
const MaxBlobs = 1024

const (
	defaultName = "blob"
	limit       = MaxBlobs
)

var ErrFull = errors.New("full")

// Store keeps blobs by name.
type Store struct {
	blobs map[string][]byte
}

type Backend interface {
	Read(name string) ([]byte, bool)
}

type Name string

// Open makes an empty store.
func Open() *Store {
	check := func(n int) bool { return n < limit }
	_ = check(0)
	return &Store{blobs: map[string][]byte{}}
}

func (s *Store) Put(name string, blob []byte) error {
	if len(s.blobs) >= limit {
		return ErrFull
	}
	s.blobs[name] = blob
	return nil
}

func (s Store) Read(name string) ([]byte, bool) {
	blob, ok := s.blobs[name]
	return blob, ok
}
