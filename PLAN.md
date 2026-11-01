# hypermag — a high-throughput BitTorrent engine in Rust

Status: draft v0.1
Goal: download (and later seed) torrents as fast as the link, disk and swarm allow,
with bounded memory and predictable CPU cost.

---

## 1. Objective and success criteria

### Primary objective

Saturate the available network path for a single torrent, then scale to many
concurrent torrents without collapsing.

"Full bandwidth" is a system property, not a protocol feature. The engine must
keep enough requests in flight to cover the bandwidth-delay product (BDP), keep
the hasher and disk off the network reactor, and keep only peers that actually
deliver.

### SLOs (acceptance targets)

| Metric | Target |
|---|---|
| Single torrent, 2-node local swarm | >= 90% of `iperf3` TCP throughput |
| Single torrent, public swarm, enough peers | saturate access link (1 Gbps reference) |
| CPU per 1 Gbps payload | <= 1 core total (net + hash + disk) |
| Steady-state memory, 200 peers | <= 256 MiB, no growth over 24 h |
| Time to first useful byte, magnet w/ DHT | < 2 s on a warm network |
| Data integrity | byte-identical output, every piece SHA-1 verified |
| Restart | resume from fast-resume in < 1 s, no re-hash of complete pieces |
| Correctness under abuse | no panic/OOM on malformed peer input (fuzzed) |

### Non-goals (v1)

- GUI, media streaming/transcoding, RSS automation.
- Ratio cheating, private-tracker evasion.
- Windows/macOS parity in the first milestone (Linux first, portable core).

---

## 2. Performance model: where the bandwidth actually goes

A naive client is not slow because TCP is slow. It is slow because of these, in
rough order of impact:

| # | Bottleneck | Symptom | Mitigation |
|---|---|---|---|
| 1 | Too few bytes in flight (BDP) | single peer caps at ~10–50 Mbps on high-RTT links | adaptive request pipelining, ~16 KiB blocks, depth = BDP/16 KiB |
| 2 | Too few / bad peers | aggregate stalls, long tail | 50–200 active peers, peer scoring, churn control |
| 3 | Hashing on the reactor | one core pegged, jitter, stalls | dedicated hash pool, incremental SHA-1, SHA-NI |
| 4 | Disk writes in the hot path | backpressure into sockets | batched `pwritev`, `fallocate`, io_uring, no fsync per block |
| 5 | Per-message allocation/copy | high allocator + memcpy cost | `BytesMut`/`Bytes`, zero-copy parse, buffer reuse |
| 6 | Shallow read sizes / syscalls | CPU burn at high pps | 64–256 KiB reads, parse all complete messages per read |
| 7 | Choking strategy | peers choke us | tit-for-tat, generous unchoke slots, upload to get download |
| 8 | Endgame straggler | last 1% takes forever | duplicate requests + cancel |
| 9 | NAT / no inbound | fewer, slower peers | UPnP/NAT-PMP/PCP, IPv6, listen port |
| 10 | Fragmented piece picking | many partial pieces, memory + tail | partial-piece affinity, rarest-first |

### BDP math (the single most important tuning knob)

```
BDP        = link_bandwidth * RTT
depth      = ceil(BDP / 16 KiB)     # outstanding block requests per peer
```

| Link | RTT | BDP | Outstanding 16 KiB blocks |
|---|---|---|---|
| 1 Gbps | 50 ms | 6.25 MB | ~400 |
| 1 Gbps | 10 ms | 1.25 MB | ~80 |
| 10 Gbps | 50 ms | 62.5 MB | ~4000 |

The client must measure per-peer RTT and throughput (Linux `TCP_INFO`, or EWMA of
request→block latency), then grow/shrink the request window. Fixed pipelining
depth is the classic reason "gigabit" clients top out at 30 MB/s.

### Throughput budget at 1 Gbps (125 MB/s)

- ~7,600 blocks/s at 16 KiB
- ~7,600 inbound block messages/s + control traffic
- SHA-1: 125 MB/s, trivially within one modern core (SHA-NI does 1–3 GB/s)
- Disk: 125 MB/s sequential, trivial for NVMe/SSD, fine for HDD with batching

At 10 Gbps: 76k blocks/s, 1.25 GB/s hash, 1.25 GB/s disk. This is where
per-message overhead, hashing parallelism and io_uring start to matter.

---

## 3. Protocol scope

### v1 (must have)

- BEP 3 — core peer wire protocol
- BEP 20 — peer ID convention
- BEP 23 — compact peer lists
- BEP 6 — fast extension (have all/none, reject, allowed-fast, suggest)
- BEP 10 — extension protocol
- BEP 9 — `ut_metadata` (magnet → metadata)
- BEP 11 — PEX
- BEP 5 — DHT
- BEP 15 — UDP tracker
- HTTP(S) trackers (BEP 3 announce/scrape, compact)
- BEP 19 — HTTP/FTP web seeds (GetRight) and BEP 17 where useful
- BEP 27 — private torrents (disable DHT/PEX)
- v1 torrents (SHA-1, 16 KiB blocks / piece hashes)

### v2 / deferred (design for, ship later)

- BEP 52 — BitTorrent v2: SHA-256, 16 KiB blocks, Merkle trees, piece layers,
  hybrid v1+v2 torrents. Big but increasingly common; keep hashing and metadata
  abstractions generic from day one.
- MSE/PE encryption (de facto, not a BEP) — some peers require it.
- BEP 29 — uTP over UDP (NAT-friendly). Significant complexity; TCP first.
- BEP 14 — LSD (local peer discovery) — cheap, optional.
- BEP 7/24/25 etc. — IPv6 tracker extensions, external IP, tracker tiers.
- Super-seeding (BEP 16) — only relevant when seeding.

### Deliberately out of scope

- DHT scraping/indexing, magnet search, web UI, remote API.

---

## 4. Architecture

### Workspace layout

```
hypermag/
├── Cargo.toml                  # workspace
├── crates/
│   ├── hypermag-bencode/       # zero-copy bencode decode, strict encode
│   ├── hypermag-meta/          # .torrent, magnet, info-hash v1/v2, file tree
│   ├── hypermag-proto/         # handshake, wire messages, ext protocol, MSE
│   ├── hypermag-picker/        # piece/block state machine, endgame, priorities
│   ├── hypermag-storage/       # preallocate, block writes, hash verify, resume
│   ├── hypermag-net/           # listeners, dialer, transports (TCP, later uTP)
│   ├── hypermag-tracker/       # HTTP + UDP tracker clients
│   ├── hypermag-dht/           # routing table, get_peers/announce_peer
│   ├── hypermag-engine/        # swarm orchestration, choking, stats, policy
│   └── hypermag-cli/           # binary
└── tests/                      # interop + local swarm harness
```

Dependency direction: `bencode`, `meta`, `proto`, `picker` are pure and
runtime-free. `storage` depends on `meta`. `engine` composes everything. `cli` is
thin. This makes the picker and parsers unit-testable without sockets or tokio.

### Runtime model

Start with **tokio multi-thread** + explicit thread pools. It is mature, has the
best ecosystem, and gets to 1–10 Gbps when the hot path is designed correctly.

```
                 ┌─────────────────────────────────────────┐
  trackers ─────▶│            engine / coordinator          │
  DHT ──────────▶│  peer set · picker grants · choking      │
  PEX ──────────▶│  stats · policy · rate control           │
                 └───────┬───────────────┬─────────────────┘
                         │ batch grants  │ events
        ┌────────────────┴───────┐   ┌───┴──────────────────┐
        │  N network workers     │   │  metrics / tracing   │
        │  (tokio tasks, 1/peer) │   └──────────────────────┘
        └───────┬────────────────┘
                │ blocks (bounded)
        ┌───────▼────────┐   ┌──────────────────┐   ┌──────────────┐
        │ hash workers   │◀──│  piece assembly  │──▶│ disk workers │
        │ SHA-1 / SHA-256│   │  (bounded slots) │   │ pwrite / uring│
        └───────┬────────┘   └──────────────────┘   └──────────────┘
                │ verified pieces
        ┌───────▼────────┐
        │ picker / state │  (authoritative, single writer or sharded)
        └────────────────┘
```

Concurrency rules:

- **One task per peer connection.** Never one task per block or per message.
- **Bounded channels everywhere.** Backpressure must reach the sockets, not the
  allocator.
- **The picker is the serialization point.** Do not lock it per block. Peer tasks
  ask for a *batch* of requests (up to their window) and report completed blocks
  in batches. Shard by torrent; within a torrent, keep picker mutations in one
  coordinator task or a `parking_lot::Mutex` held for microseconds.
- **No blocking work on the reactor**: hashing, fsync, tracker HTTP, DNS,
  bencode of large metadata all go to pools.
- **Backpressure policy**: if disk or hash queues exceed high-water marks, stop
  granting new requests and shrink peer windows.

### Escalation path for >10 Gbps / many torrents

Keep transport and storage behind traits so we can later swap in:

- `monoio` or `glommio` (thread-per-core, io_uring), or raw `io-uring` with a
  custom driver;
- `SO_REUSEPORT` listeners sharded by 4-tuple hash;
- one engine shard per core, torrents pinned to shards.

Do not start here. It buys maybe 2–3x at the top end and costs weeks of
complexity. Measure first.

---

## 5. Hot data path

```
socket recv (64 KiB buf)
  → BytesMut accumulator
  → parse all complete messages
  → MessageRef / BlockRef (zero-copy slices, refcounted Bytes)
  → peer window accounting
  → block handed to piece slot (bounded pool)
  → write block to disk immediately (pwrite, offset = piece*len + begin)
  → when piece has all blocks: hash from in-memory slot
  → on match: mark verified, free slot, notify picker + peers (Have)
  → on mismatch: mark piece failed, re-request, penalize senders
```

Key choices:

- **Write blocks immediately, verify per piece.** Avoids accumulating whole
  pieces in a second buffer; bounds memory to the number of in-flight partial
  pieces. Do not hash from disk on the hot path (extra read amplification);
  hash the in-memory slot before freeing it. Optional "hash from disk" mode for
  memory-constrained targets.
- **Cap partial pieces.** e.g. 8–32 concurrent partial pieces per torrent;
  picker prefers finishing already-partial pieces. This also improves
  streaming/sequential behavior.
- **Requests**: fixed 16 KiB blocks, pipelined; concatenate multiple request
  messages into one write when possible; `TCP_NODELAY` on.
- **Receives**: read big, parse many, avoid one syscall per 16 KiB message.
- **Zero-copy where cheap**: `Bytes` for payload; avoid copying block bytes more
  than once (recv buffer → piece slot → disk).
- **Per-peer state is small**: window, rates, RTT EWMA, bitfield, request queue.
  Use compact bitfields and slab/arena allocation for block slots.

### Memory budget (per torrent, tunable)

| Item | Budget |
|---|---|
| Partial piece slots (32 × 4 MiB) | 128 MiB |
| Per-peer buffers (200 × 64 KiB) | 12.8 MiB |
| Pending message queue | 8 MiB |
| Metadata / bitfields / picker | < 8 MiB |
| **Total** | **~160 MiB** |

Piece size is negotiated from metadata; clamp partial-piece count so
`pieces_in_flight * piece_size <= budget`.

---

## 6. Piece picker (the real differentiator)

State machine per piece:

```
MISSING ──pick──▶ PARTIAL(bitmap) ──all blocks──▶ VERIFYING ──ok──▶ COMPLETE
   ▲                    │                             │
   └────requeue─────────┘                             └──fail──▶ MISSING (re-download)
```

Requirements:

1. **Rarest-first** across pieces we don't have, weighted by availability.
2. **Partial-piece affinity**: strongly prefer pieces already in progress, to
   finish them and free memory.
3. **Priority tiers**: first/last piece first (for preview), user file
   priorities, sequential mode option.
4. **Endgame mode**: when remaining blocks are few, request duplicates from
   multiple peers; cancel on first arrival.
5. **Per-block deadlines**: `deadline = now + k * RTT + slack`; requeue to
   another peer, penalize non-responders.
6. **Duplicate suppression**: one outstanding request per block unless endgame.
7. **Availability tracking**: maintain per-piece availability counts from
   bitfields (increment/decrement on peer add/remove/have) with a bucketed
   index so "rarest" is O(1)-ish, not a scan of all pieces.
8. **Consistency**: all mutations through the picker owner; expose batch
   `grant(peer, max_blocks) -> Vec<BlockRequest>` and `complete(block)`.
9. **Fairness**: no single peer starves; window-based grants.
10. **Resume**: serialize picker state (piece states + block bitmaps) as
    fast-resume.

This module deserves the most tests: model-based/property tests comparing
against a reference state machine, plus invariant checks (no double-complete, no
request for a complete block, memory bound respected).

---

## 7. Peer lifecycle and choking

### Discovery

- Trackers (HTTP/UDP) with tier backoff; DHT; PEX; incoming connections; LSD.
- Deduplicate by peer ID and by IP:port; ignore self and own listen port.
- Dial budget: max half-open connections (e.g. 30/s, 100 outstanding), backoff
  per peer, don't hammer dead peers.

### Connection policy

- Target 50–200 active peers per torrent, 400 total across torrents (tunable).
- Keep peers that deliver; drop peers that are choked + idle for N seconds, or
  whose EWMA rate is far below the swarm median when we're at peer cap.
- Prefer peers with high availability for rare pieces; keep some "lottery"
  peers for discovery.
- Incoming peers are valuable: port mapping + IPv6.

### Choking (tit-for-tat)

- Recompute every 10 s: unchoke top 4 uploaders by reciprocation rate, rotate
  one optimistic unchoke every 30 s.
- Upload slots are not charity: BitTorrent reciprocation means giving upload
  buys download. Set generous but bounded slots (e.g. 8–16) at high bandwidth.
- Snub peers that don't deliver; track request timeouts.
- Seed mode: round-robin + rare-piece priority, optional super-seeding.

### Keep-alive / liveness

- Keep-alive every 2 min; drop after ~2.5 min silence; stagger timers to avoid
  thundering herds.

---

## 8. Network tuning checklist

Socket level:

- `TCP_NODELAY`, keepalive on, `SO_RCVBUF`/`SO_SNDBUF` sized to ~BDP.
- Linux `TCP_INFO` for RTT/cwnd/retransmits → feed the adaptive window.
- Optional `TCP_CONGESTION=bbr` for seeding (document, don't force).
- Listen on dual-stack; UPnP IGD / NAT-PMP / PCP mapping with renewal.
- `SO_REUSEPORT` only when sharding listeners.

Application level:

- Adaptive request depth per peer:
  `depth = clamp(ceil(bdp/16KiB), 16, 1000)`, grow on timely blocks, shrink on
  timeouts.
- Read sizes 64–256 KiB; parse loop per read.
- Batch small control messages into a single write buffer.
- Rate limits default off; implement token buckets at the swarm level, not per
  message.
- Timeouts: connect 10 s, handshake 10 s, request deadline function of RTT.
- Avoid head-of-line blocking: one slow peer must not stall the picker; grants
  are per peer.

OS level (document in a `docs/tuning.md`):

- `net.core.rmem_max`, `net.core.wmem_max`, `net.ipv4.tcp_rmem/wmem` for high
  BDP links; otherwise the kernel caps window growth.
- `ulimit -n` ≥ 4096; `somaxconn`.

---

## 9. Storage and hashing

### File layout

- Parse `info.files` into a flat list of `(path, length, offset)`; support
  multi-file torrents and files > 4 GiB.
- Map piece → list of `(file, offset, len)` segments. Keep the mapping allocation
  free; precompute boundary index and binary search.
- Preallocate with `set_len` + `fallocate(PUNCH_HOLE|KEEP_SIZE)` or sparse,
  configurable.
- Open files lazily with a small LRU fd cache; cap open fds.

### Writes

- `BlockStore` trait:
  `write_blocks(&mut self, &[BlockRef]) -> Result<...>` and
  `read_piece(...)`, `flush(...)`.
- First implementation: `spawn_blocking` + `pwritev` batches (group blocks by
  contiguous offset). No `fsync` per block.
- Second implementation: io_uring batched writes (`IOSQE_IO_LINK`, deep queue),
  one ring per disk worker. Measure vs baseline.
- Never write through a shared `File` lock from many tasks.

### Hashing

- SHA-1 via `sha1` with SHA-NI/asm, benchmark on target hardware.
- Hash pool sized `max(1, cores/4)`; pieces are independent → parallel.
- For small pieces and very high throughput, consider multi-buffer SHA-1.
- v2: SHA-256 + Merkle; keep `Hasher` trait with `hash_piece`.
- Verify before marking complete; on failure, requeue piece and record which
  peers contributed (penalty).

### Resume

- Fast-resume: piece/block bitmaps + file sizes/mtimes + info-hash; validate
  cheaply, never re-hash verified data.
- On graceful shutdown: flush, write fast-resume atomically (temp + rename).
- Optional periodic checkpoint.

---

## 10. Observability and benchmarking

Metrics (per torrent and global):

- payload rate in/out, protocol overhead ratio
- peers: connected/half-open/choked/unchoked, peer source
- requests in flight, grant latency, block latency p50/p99, timeouts
- picker: complete/partial/missing, endgame active, duplicate rate
- hash queue depth/latency, disk queue depth/latency, write throughput
- socket: RTT, cwnd, retransmits (Linux)
- memory: slots used, buffer pool, RSS

Tooling: `tracing` + `tracing-subscriber`, `metrics` crate with Prometheus
exporter, `tokio-console` in dev, `pprof`/`flamegraph` for CPU.

Benchmark harness (part of the repo, reproducible):

1. Local swarm in Docker: 1 leecher + N seeders on a bridge; baseline `iperf3`.
2. `tc netem` for RTT (10/50/150 ms) and loss (0/0.1/1%).
3. Small (1 GiB), medium (20 GiB), many-files (10k files) torrents; piece sizes
   256 KiB–4 MiB.
4. Report: throughput vs baseline, CPU seconds/GB, RSS, tail time for last 1%.
5. Soak: 24 h, watch RSS/fd/queue growth.

Reference implementations for interop and as an oracle: qBittorrent/libtorrent,
Transmission, rqbit. Benchmark against them on the same host/swarm.

---

## 11. Testing strategy

- **Unit**: bencode (round-trip, malformed), metainfo, wire codec, storage
  offset math, picker invariants.
- **Property/model tests**: picker vs reference model; random grant/complete/
  disconnect sequences; assert no duplicate completion, bounded memory, progress.
- **Fuzzing** (`cargo-fuzz`): bencode, handshake, message stream parser, extension
  messages, MSE handshake. Every parser is adversarial input.
- **Interop matrix** (CI nightly): each client as seeder and as leecher.
- **Chaos**: peers disconnect mid-block, send garbage, lie in bitfields, hash
  failures, duplicate blocks, slow-loris.
- **E2E**: generated random data, never copyrighted content.
- **Soak/leak**: 24 h with churn.

---

## 12. Milestones

Estimates assume one experienced Rust dev; adjust, but keep the order.

### M0 — Skeleton (1–2 days)
- Workspace, CI (fmt/clippy/test), error types, `tracing`, CLI arg parsing.
- Bencode decode/encode with tests and fuzz target.
- Parse `.torrent`: info-hash v1, piece length, files, trackers.
- **Exit**: `hypermag inspect file.torrent` prints a correct summary.

### M1 — One peer, one torrent (3–5 days)
- TCP dial + handshake, bitfield, interested/unchoke, request/piece/cancel.
- Piece store: preallocate, pwrite blocks, SHA-1 verify, write output.
- Fixed pipeline depth; no picker intelligence yet.
- **Exit**: download a torrent from a local `transmission-cli`/`qbittorrent-nox`
  seeder, byte-identical, verified.

### M2 — Swarm (1–2 weeks)
- HTTP + UDP trackers, compact peers, peer manager, dial budget.
- Real picker (rarest-first, partial affinity, endgame, deadlines).
- Choking/unchoking, keep-alive, peer scoring, connection caps.
- Bounded queues + backpressure.
- **Exit**: 20 GiB torrent from 5 local seeders; correct; no leaks; >= 70% of
  iperf3 baseline at 10 ms RTT.

### M3 — Performance (1–2 weeks)
- Adaptive request windows via RTT/rate; `TCP_INFO`.
- Batched reads/writes, buffer pools, zero-copy parse.
- Hash pool + disk worker tuning; io_uring prototype behind trait.
- Profiling: flamegraph, per-core utilization.
- **Exit**: >= 90% of iperf3 baseline on 1 Gbps at 50 ms; <= 1 core/Gbps;
  memory within budget; 10 Gbps loopback experiment documented.

### M4 — Discovery (1 week)
- DHT (BEP 5), PEX (BEP 11), magnet + `ut_metadata` (BEP 9/10).
- **Exit**: magnet-only download works on a public legal torrent; peers found
  by DHT/PEX with trackers disabled.

### M5 — Reach and resilience (1–2 weeks)
- MSE/PE encryption, UPnP/NAT-PMP/PCP, IPv6, LSD.
- **Exit**: connects to encryption-required peers; inbound peers observed
  behind NAT.

### M6 — v2, resume, hardening (2 weeks)
- BEP 52 (v2 + hybrid), fast-resume, file priorities, seeding policy.
- Fuzz corpus in CI, chaos tests, soak.
- **Exit**: hybrid torrent downloads correctly; crash → resume without re-hash.

### M7 — Benchmark and docs
- Reproducible benchmark report, tuning guide, architecture doc.
- **Exit**: numbers published against baseline clients on identical hardware.

---

## 13. Key decision records (to confirm)

| Decision | Recommendation | Why |
|---|---|---|
| Runtime | tokio first, traits for swap | maturity; enough for 10 Gbps |
| Disk backend | pwrite first, io_uring second | measure before complexity |
| Hash placement | in-memory slot before free | avoids read amplification |
| Verify timing | per piece, before marking complete | correctness first |
| Piece buffer budget | ~128 MiB/torrent default | bounds RSS |
| Peer cap | 200 active/torrent, 400 global | CPU/churn vs availability |
| Encryption | phase M5, not v1 | perf + complexity |
| uTP | after TCP is fast | large effort, not needed for max speed on good links |
| v2 | M6, abstractions from day 1 | ecosystem is moving to hybrid |
| Seeding | same engine, policy separate | needed for reciprocation anyway |

### Open questions

1. Is the target a **CLI client**, a **library**, or both?
2. Linux-only acceptable (io_uring, `TCP_INFO`), or cross-platform required?
3. Must it **seed** (affects choking, ratio, super-seeding)?
4. Single-torrent speed is the priority, or many concurrent torrents?
5. Any hard constraints: no unsafe, no C deps, binary size, static musl?
6. Is v2/hybrid support required early (many modern torrents are hybrid)?

---

## 14. Immediate next actions

1. Answer the open questions above; freeze v1 scope.
2. `cargo new` workspace with the crate skeleton and CI.
3. Implement `hypermag-bencode` + `hypermag-meta` with tests/fuzz (M0).
4. Stand up the local swarm harness early — it is the feedback loop for every
   later performance decision.
5. Write the picker as a pure state machine with model tests before wiring
   sockets (M1–M2).
