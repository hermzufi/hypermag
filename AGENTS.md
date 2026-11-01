# hypermag

High-throughput BitTorrent engine in Rust. M0 only: workspace, strict zero-copy
bencode, `.torrent`/magnet parsing, inspection CLI. Downloading is M1+; most
crates under `crates/` are stubs.

## Toolchain

- Rust stable (pinned by `rust-toolchain.toml`: components `rustfmt`, `clippy`).
- Workspace MSRV `1.85`, edition `2024`. `#![forbid(unsafe_code)]` at workspace level.
- If `cargo` is not on `PATH`, run `source ~/.cargo/env`.

## Commands

```sh
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
cargo run -p hypermag-cli -- inspect path/to/file.torrent
cargo run -p hypermag-cli -- inspect --files path/to/file.torrent
cargo run -p hypermag-cli -- inspect --json path/to/file.torrent
cargo run -p hypermag-cli -- magnet 'magnet:?xt=urn:btih:...'
```

Run a single crate or test:

```sh
cargo test -p hypermag-meta
cargo test -p hypermag-meta parses_magnet_hex
cargo test -p hypermag-cli
```

Fuzzing (separate workspace under `fuzz/`, excluded from main workspace; needs
nightly + `cargo install cargo-fuzz`):

```sh
cd fuzz
cargo +nightly fuzz run bencode
cargo +nightly fuzz run metainfo
cargo +nightly fuzz run magnet
```

CI (`.github/workflows/ci.yml`) runs `fmt --check`, clippy `-D warnings`,
`test`, `doc` on push/PR to `main`, with `RUSTFLAGS=-D warnings`. Keep all
warnings clean.

## Workspace layout

- `crates/hypermag-bencode` — zero-copy bencode codec; spans for raw `info`.
- `crates/hypermag-meta` — `.torrent`, magnet, info-hash v1/v2.
- `crates/hypermag-proto` — peer wire codec (BEP 3/10); `crates/hypermag-storage` — piece/file layout + SHA-1 verify; `picker|net|tracker|engine|dht` — stubs (M1+).
- `crates/hypermag-cli` — `hypermag` binary; integration tests in `tests/cli.rs`.
- `fuzz/` — separate Cargo workspace (excluded), targets bencode/meta/magnet.

Dependency direction: `bencode`, `meta`, `proto`, `picker` are pure and
runtime-free; `storage` depends on `meta`; `engine` composes; `cli` is thin.

## Key conventions / gotchas

- Zero-copy borrows: `Metainfo<'a>` and `Info<'a>` borrow the input bytes
  (`pieces: &'a [u8]`, `info_bytes: &'a [u8]`). Keep source bytes alive for the
  lifetime of parsed metadata.
- Info-hash v1 is SHA-1 over the *exact* bytes of the `info` dict as they
  appeared in input. Use `Decoder::parse_dict_with_spans_complete()` and the
  `DictEntry::span`, do not re-encode (canonical re-encoding may differ).
- Bencode decoder is strict: rejects leading zeros, `-0`, duplicate keys,
  unbounded nesting (default max depth 128), trailing bytes. Dictionary key
  order is *not* enforced on decode; encoder always sorts keys.
- CLI tests invoke the built binary via `env!("CARGO_BIN_EXE_hypermag")` and
  write temp files to `std::env::temp_dir()`.
- `cargo test --workspace` does not build/run fuzz targets (fuzz excluded).
