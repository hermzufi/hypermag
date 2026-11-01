# hypermag

A high-throughput BitTorrent engine in Rust. The design and roadmap live in
[`PLAN.md`](./PLAN.md).

## Status

Milestone **M0**: workspace, strict zero-copy bencode, `.torrent` and magnet
parsing, and an inspection CLI. Downloading arrives in M1.

## Quick start

```sh
cargo run -p hypermag-cli -- inspect path/to/file.torrent
cargo run -p hypermag-cli -- inspect --files path/to/file.torrent
cargo run -p hypermag-cli -- inspect --json path/to/file.torrent
cargo run -p hypermag-cli -- magnet 'magnet:?xt=urn:btih:...&dn=name'
```

Example output:

```
Name:         ubuntu-24.04.iso
Info hash:    2f3e... (v1)
Magnet:       magnet:?xt=urn:btih:2f3e...&dn=ubuntu-24.04.iso
Total size:   5.70 GiB (6123456789 bytes)
Piece length: 256.00 KiB
Pieces:       23359
Private:      no
Files:        1
Trackers:     2
  udp://tracker.example:80
Created by:   mktorrent
```

## Workspace

| Crate | Role | Status |
|---|---|---|
| `hypermag-bencode` | strict zero-copy bencode codec, spans for raw `info` hashing | done |
| `hypermag-meta` | `.torrent`, magnet, info-hash v1/v2 | done |
| `hypermag-proto` | peer wire protocol, extensions | M1 |
| `hypermag-storage` | block writes, SHA-1 verification, resume | M1 |
| `hypermag-net` | dialer, listener, transports | M1 |
| `hypermag-picker` | rarest-first piece picker, endgame | M2 |
| `hypermag-tracker` | HTTP/UDP trackers | M2 |
| `hypermag-engine` | swarm orchestration, choking, windows | M2 |
| `hypermag-dht` | mainline DHT | M4 |
| `hypermag-cli` | `hypermag` binary | M0 |

## Development

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Fuzzing (requires nightly and `cargo install cargo-fuzz`):

```sh
cd fuzz
cargo +nightly fuzz run bencode
cargo +nightly fuzz run metainfo
cargo +nightly fuzz run magnet
```
