# socket-core

Who a link socket's stand-ins stand for, and where each peer's packets go.
A profile has one UDP socket under quinn, and quinn never sees the network:
it sends to a stand-in address, one per peer, and the socket puts each
packet on that peer's carrying path (straight to an address, through a
forwarding link behind a header naming the peer, or over a byte stream).
Arrivals are mapped back to the peer's stand-in whichever path brought them,
so a path change is invisible to quinn.

`Peers` holds every decision that takes: which peer an arrival came from,
which path a transmit goes out on, which paths a dial goes out on, what a
dial's first flight has sent so far and where it must go again once an
arrival or a forwarding link moves the peer. A transmit or an arrival goes
in with the time, and what to send where comes out; the node's link socket
([`link/socket.rs`](../node/src/link/socket.rs)) does the sending and holds
the byte streams, and the [socket model](../models/README.md#the-link-socket)
explores these same decisions over a world of its own. The crate also reads
and writes the socket's own datagrams: the forwarding header, probes and
their answers.

The rules it keeps:

- A stand-in names one peer at a time. A sender nobody has named yet is
  an unnamed peer of its own until its handshake names the host, and one
  left unnamed or silent for a minute is let go. A host this host's own
  dial is at keeps its stand-in when a connection from it is named
  elsewhere, since a dialled connection hears only the address it dialled;
  the named connection, an accepted one, moves to it.
- A first flight's route is not trusted until a link comes up on it: the
  path it settled on is only the first to answer, which may have turned
  the dial away.
- A dial's first flight goes out on every path the dial chose, and a packet
  of it is sent again wherever an arrival or a forwarding link moves the
  peer before the handshake completes, so a flight never strands on a path
  nobody listens on.
- A handshake arriving through a forwarder while another path carries its
  peer moves the peer there, since a host started afresh may know no other
  way back; the path machine moves the link back once the other path
  answers.
- A probe's answer from an address one of this host's dials is still using
  is not mistaken for a stray from the peer's earlier run.
- A forwarder is kept however long it is silent while a link it forwards
  is up: the node removes the link, and the routes through it go with it.

Like the cores, it locks, spawns, awaits and reads the clock nowhere: the
time is an input.
