# Probe file format specification, v1 and v2

Status: **stable.** Probe v1 and Probe v2 are frozen: the meaning of every byte is fixed. This
document is normative. A reader written from it alone decodes every Probe v1 and v2 file, and
proves it on the conformance vectors of section 8.

| Probe | header `"version"` | magic | status |
|---|---|---|---|
| **v1** | `"evmin/4"` | `EVM4` | frozen 2026-11-12 |
| **v2** | `"evmin/5"` | `EVM4` | current |

The version strings keep the format's working name, *evmin* (event log, minimal). The magic is
`EVM4` for both versions; only the header's `version` tells them apart. Files are named
`*.probe` by convention; the extension carries no meaning.

**Conventions.** MUST, MUST NOT, SHOULD and MAY are used as in RFC 2119. Integers in the container
are little-endian. `uv` is an unsigned LEB128 varint, at most 10 bytes. `zz` is zigzag:
`zz(v) = 2v` for `v >= 0`, `-2v - 1` for `v < 0`; `unzz(u) = (u >> 1) XOR -(u AND 1)`. `floor` is
floor division. "micro" means units of 1e-6.

Measured numbers carry their source in square brackets:

- **[capture]**: a 15-minute capture of 20 busy markets, 873,781 level changes, measured on a
  desktop PC loaded by other work (timings there are upper bounds);
- **[all-markets sample]**: a 10-minute window of every market of the venue;
- **[production hour]**: one full hour, 2026-11-12 10:00-11:00 UTC, every market, a v1 file of
  533,465,796 bytes;
- **[benchmark]**: the decode benchmark of section 7.3, on a cloud VM with 8 vCPUs.

---

## 1. Overview

### 1.1 What a Probe file is

One Probe file holds **one UTC hour of one venue (Polymarket), for every market the writer
follows**. It contains:

- every change of every price level of every outcome token's order book, at full depth, before any
  throttling or sampling, in arrival order;
- every trade (`last_trade_price`) with its fee rate and transaction hash;
- every tick-size change, and every other market-channel event as its JSON text;
- two clocks on the events: the writer's receive time at 1 microsecond resolution, and the venue's
  own timestamp in milliseconds. A checkpoint (type 5) has no clock. A type-4 event has no venue
  clock: its venue time is the receive time in ms;
- a full-book checkpoint of each market at the start of each 5-minute segment in which that market
  had any event (a book change, a trade or a tick-size change), plus one for every non-empty book in
  the first segment each writer process enters in the hour (3.8).

The production hour, counted several ways:

| what is counted | count |
|---|---|
| **level changes**: price_change entries minus mirrors, plus book-diff levels | **61,625,246** = 123,199,506 entries - 61,599,753 mirrors + 25,493 book-diff levels |
| real levels (the header's `n_lv`, summed): every level of a real event | 123,268,560 |
| every decoded level, checkpoint levels included | 163,913,630 = 123,268,560 + 40,645,070 checkpoint levels |
| real events (the header's `n_ev`, summed) | 61,736,026: 61,599,753 price_change, 90,376 book, 43,561 trades, 2,336 tick-size, 0 type 4 |
| every decoded event, checkpoints included | 63,621,962 = 61,736,026 + 1,885,936 checkpoints |

So the hour is about **8.66 bytes per level change**, or **3.25 bytes per decoded level**. The
full-book checkpoints are a large part of it: 40.6 M checkpoint levels against 61.6 M level
changes. The capture of 20 busy markets is 3.55 B per level change [capture].

### 1.2 Design goals, in priority order

1. **Lossless.** Every level change, trade and market event the venue delivered can be recovered
   exactly, with both clocks. The fields the format drops are only derivable ones (4.6).
2. **Compact.** Bytes per level change as low as the venue's information allows: 3.31 B core and
   3.55 B in total [capture], 8.66 B on the production hour, checkpoints included.
3. **One market is readable without reading the file.** The header, the market table and a
   segment's block index locate one market's bytes. In v2 they also locate its trades' hashes and
   payloads. A reader fetches only those byte ranges.
4. **Any 5-minute boundary is a valid start.** The book of any token at any time needs one block
   decoded from its start. No decoder state crosses a block boundary.
5. **Self-describing.** The header is JSON. It names the version, the coding parameters, every
   part's position, and every market's identity and units. No outside table is needed to turn
   integers into prices, sizes, tokens or times.
6. **Deterministic bytes.** The same input to the same writer gives the same file, byte for byte.
   No write time or host name is stored.

### 1.3 Non-goals

- **Not a wire protocol.** Probe is a file format for closed hours. It is not streamed and has no
  framing for live delivery. A file is written once, after its hour ends.
- **No order-by-order book.** Polymarket publishes price levels, not orders. The format records
  levels and trades; it has no order ids.
- **Decode speed is traded for size.** Blocks are coded bit by bit with an adaptive binary range
  coder. They cannot be read in place like fixed-size records. Section 7.3 gives the measured
  trade-off.
- **Not a general container.** One venue, two-outcome markets, fixed parameters (3.3). A
  different venue or market shape needs a new version.

---

## 2. Versions and compatibility

### 2.1 The versions

**Probe v1 (`evmin/4`).** In v1 the side streams (`tx`, `payload.zst`, `hash`) follow the
segment's **global** arrival order. Placing one market's trade hashes therefore needs every block
of the segment decoded.

**Probe v2 (`evmin/5`).** v2 is v1 with two changes and nothing else:

1. Each block-index entry has **8 uvarints** instead of 2. The six new ones are the block's
   counts: events, levels, trades, payloads, hash items and best-price exceptions.
2. The side streams are in **block order**, and the best-exception rows use **block-relative**
   event indexes. A block's slice of each side stream is then the prefix sum of the counts before
   it.

Everything else is v1, byte for byte in meaning: the container, header keys, market table, order
stream, block bytes, checkpoints, units, checkpoint bitmap and `unsynced` list. A v2 file decodes to
exactly the same events as the v1 file of the same input; the vectors `basic_v1.probe` and
`basic_v2.probe` are such a pair. On the all-markets sample the v2 file is 13.71 MB against
17.65 MB as v1, because the block-ordered exception rows compress better.

**Earlier files.** A prototype wrote files with the same magic and a numeric `"version": 1`. They
are not Probe files.

### 2.2 Compatibility policy

1. **A frozen version never changes.** The meaning of every byte of v1 and v2 is fixed.
2. **Readers MUST reject an unknown version.** A reader MUST check the magic (`EVM4`) and MUST
   refuse any `version` other than the strings it implements (`"evmin/4"`, `"evmin/5"`). It MUST
   NOT guess. The prototype's numeric `1` is refused too.
3. **Readers MUST refuse other coding families.** A file whose `family` is not `"rc"`, or whose
   `layout` is not `"mkt"`, MUST be refused.
4. **Additive fields within a version.** A writer MAY add a JSON key to the header, a segment
   object or a market row, but only if a reader that ignores the key still decodes every event,
   level, clock and side-stream item exactly as before. Readers MUST ignore JSON keys they do not
   know, and MUST locate parts by name, ignoring part names they do not know. Anything that
   changes how a byte decodes needs a new version.
5. **Upgrading.** A new version gets a new `version` string. It keeps the 8-byte prefix (magic
   plus header length) so that old readers can read the version and refuse the file cleanly. The
   new version's section MUST list every change against the previous version. It MUST ship
   conformance vectors that decode to the previous version's reference output wherever the
   content is the same, as `basic_v2.probe` does for v1. Readers SHOULD keep decoding every
   earlier version. An existing file is never rewritten; the `.evidx` sidecar (Appendix A) gives
   v1 files v2's per-block positions without changing them. The next version string is
   **`"evmin/6"`**, with the same `EVM4` magic and 8-byte prefix.

---

## 3. File layout

### 3.1 Container

    offset 0   4 bytes            magic, ASCII "EVM4"
    offset 4   u32 LE             header_len
    offset 8   header_len bytes   header JSON (UTF-8)
    offset 8 + header_len         body

Every offset in the header is relative to **the start of the body** (`8 + header_len`). The body
holds the segments' parts and the market table. A writer typically lays them out as: for each
segment in order, its parts `order`, `bidx`, `blocks`, `tx`, `payload.zst`, then `hash` when
`has_hash`; after the last segment, `markets_zst`. Readers MUST use the stated offsets. They MUST
NOT assume this order.

### 3.2 Header JSON

Writers emit compact JSON. Readers MUST NOT depend on key order or spacing.

| key | type | meaning |
|---|---|---|
| `magic` | string | `"EVM4"`, repeating the magic |
| `version` | string | `"evmin/4"` (Probe v1) or `"evmin/5"` (Probe v2) |
| `recorder` | string | identifies the writer (`"test"` in the vectors). Informative only |
| `family` | string | `"rc"`. The only value allowed |
| `layout` | string | `"mkt"` (one block per market per segment). The only value allowed |
| `unit_ns` | int | the receive-clock unit in ns. Always `1000` (1 microsecond) |
| `seg_s` | int | segment length in seconds. Always `300` |
| `shift` | int | the range coder's adaptation shift (3.10). Production: `4` |
| `flags` | int | coding-variant bits (3.11). Production: `80` (= 16 + 64) |
| `has_hash` | bool | whether segments have the optional `hash` part |
| `hour_start_ns` | int | the file's UTC hour start, ns since the epoch |
| `markets_zst` | [int, int] | `[offset, length]` of the market table in the body |
| `segments` | array | the segments in ascending `t0_ns`, below |

Each element of `segments`:

| key | type | meaning |
|---|---|---|
| `t0_ns` | int | segment start, ns since the epoch, a multiple of 300 s. The segment covers receive times `[t0_ns, t0_ns + 300 s)`, except for late events (5.6) |
| `parts` | array of `[name, offset, length]` | the segment's parts; offsets are into the body |
| `nmkt` | int | market rows in use when the segment was written. Block ids run `0..nmkt`: `0..nmkt-1` are market rows, and `nmkt` is the market-less block of event type 4 |
| `n_ev` | int | **real** events of the segment (checkpoints excluded) = the order stream's count |
| `n_lv` | int | levels of the real events |
| `n_ck` | int | checkpoint events (event type 5) of the segment |
| `ckpt` | string | base64 (standard alphabet, padded) checkpoint bitmap over rows `[0, nmkt)` (3.8) |
| `unsynced` | array of `[row, outcome]` | checkpoints synthesized from a book that had no venue snapshot yet (3.9) |

No header field depends on the time of writing.

### 3.3 Fixed parameters

Probe v1 and v2 files are `family "rc"`, `layout "mkt"`, `unit_ns 1000`, `seg_s 300`. Production
files use `shift 4`, `flags 80`. The conformance vectors also exercise `shift 0`, `shift 5` and
`flags 0` (section 8). A reader MUST honour the header's `shift` and `flags`. A reader that does
not implement a given flag bit or shift value MUST refuse the file rather than decode it. A reader
SHOULD refuse `unit_ns` other than 1000 and `seg_s` other than 300: no such file exists.

### 3.4 Market table (`markets_zst`)

`markets_zst` is one zstd frame. Decompressed, it is a JSON array of **market rows**. A block names
its market by **row index** (0-based position in this array).

| key | type | meaning |
|---|---|---|
| `slug` | string | the venue's market slug |
| `condition_id` | string | the venue's condition id |
| `tokens` | [string, string] | the two outcome token ids. Outcome 0 = `tokens[0]`, outcome 1 = `tokens[1]` |
| `price_unit_micro` | int | one price unit in micro. Price = `Lp x price_unit_micro / 1e6` |
| `size_unit_micro` | int | one book-size unit in micro shares (event types 0, 1 and 5) |
| `trade_size_unit_micro` | int | one trade-size unit in micro shares (event type 2) |
| `U` | int | price units in 1.0: `U = 1e6 / price_unit_micro`. The mirror price of `p` is `U - p` |

**Units are per file, per row.** The writer fixes a row's units when it creates the row:

- `price_unit_micro` = gcd(10000, every price of the market's first block in the file), in micro.
  10000 micro = 0.01.
- `size_unit_micro` and `trade_size_unit_micro` = gcd(1,000,000, the sizes), in micro. 1,000,000
  micro = 1 share.

A later block can hold a value off the row's grid, for example after a `tick_size_change` to a
finer tick, or a finer size. From that segment on the writer creates a **new row**, with the same
`condition_id` and tokens and finer units. Earlier segments keep the old row, so nothing already
written is invalidated. One token can therefore sit in several rows of one file, but **within one
segment a market has at most one row**. Units can also differ from hour to hour, because each file
has its own table.

Prices are recorded in 1e-4 ticks (100 micro), and a price above 1.0 is dropped by the writer. So
`price_unit_micro` = gcd(10000, the prices) is a divisor of 10000 that is a multiple of 100: 100,
200, 400, 500, 1000, 2000, 2500, 5000 or 10000. `U` is therefore one of 10000, 5000, 2500, 2000,
1000, 500, 400, 200 or 100. For example, prices all on 0.005 give `price_unit_micro` 5000 and `U`
200. A reader MUST always convert with the row the block names.

### 3.5 Segment parts

| part | present | content |
|---|---|---|
| `order` | always | the block id of every real event, in arrival order (3.6) |
| `bidx` | always | the block index (3.7) |
| `blocks` | always | the blocks, concatenated in `bidx` order (3.8) |
| `tx` | always (may be 0 bytes) | 32-byte transaction hashes of the trades, raw (3.9) |
| `payload.zst` | always | zstd JSON `{"p": [...], "b": [...]}`: payload strings and best-price exceptions (3.9) |
| `hash` | only when `has_hash` | 20-byte venue book hashes (3.9) |

A writer MUST write `tx` and `payload.zst` (the latter holds at least `{"p":[],"b":[]}`). A reader
MAY treat an absent `tx` or `payload.zst` as empty.

### 3.6 The order stream (`order`)

    uv(n)  rc_stream_without_first_byte

`n` MUST equal the segment's `n_ev`. The stream gives the block id of each of the n real events,
in arrival order. A block id is a market row `0..nmkt-1`, or `nmkt` for an event-type-4 event.

The order stream is coded with the range coder (3.10). It has **its own model**, separate from any
block's. Its shift is the header `shift`, or 5 when the header says 0: the order stream never uses
the two-rate mix. It has two contexts:

- `SAME[2]`, a bit model indexed by the previous same bit;
- `RANK`, one gamma table (3.10.4).

Decoding:

    list = [0, 1, ..., nmkt]                  # move-to-front list, in id order
    prev_same = 0
    for each of the n events:
        same = bit(SAME[prev_same]); prev_same = same
        if same == 1: r = 0
        else:         r = gamma(RANK) + 1     # the coded value is r - 1
        id = list[r]; move list[r] to the front (entries 0..r-1 shift down one)
        emit id

The first event is compared with id 0, the front of the initial list. The header `flags` never
apply to the order stream: `prev_same` is always tracked. A rank past the list is corrupt.

The literal list costs O(nmkt) per event. A reader MAY implement the same move-to-front in
O(log n): give id `i` the initial recency slot `nmkt - i`, give each moved id a new slot above all
others, and find rank r as the (r+1)-th occupied slot from the top with a Fenwick tree. The ids
are identical.

### 3.7 The block index (`bidx`)

One entry per block, in **ascending block id**. The event-type-4 block (id `nmkt`), when present,
is therefore last.

**v1:** each entry has 2 uvarints:

    uv(row - prev_row - 1)  uv(block_length)

**v2:** each entry has 8 uvarints:

    uv(row - prev_row - 1)  uv(block_length)  uv(n_ev)  uv(n_lv)  uv(n_trade)  uv(n_pay)  uv(n_hash)  uv(n_bex)

`prev_row` is -1 at the start. A block's byte offset in `blocks` is the sum of the lengths of the
entries before it. The lengths MUST sum to the length of the `blocks` part.

The v2 counts describe the block itself:

| count | meaning |
|---|---|
| `n_ev`, `n_lv` | the block's events and levels **including** its checkpoints; repeats the block's own header |
| `n_trade` | its event-type-2 events |
| `n_pay` | its event-type-3 and -4 events |
| `n_hash` | its 20-byte hash items: 1 per type-0 event, 1 per type-1 entry (mirrors included); 0 when `has_hash` is false |
| `n_bex` | its rows in `payload.zst` `"b"` |

A block's start in each side stream is the prefix sum over the blocks before it in `bidx`:

| stream | start of the block's run | length of the run |
|---|---|---|
| `tx` | byte `32 x sum(n_trade)` | `32 x n_trade` bytes |
| `hash` | byte `20 x sum(n_hash)` | `20 x n_hash` bytes |
| `"p"` | index `sum(n_pay)` | `n_pay` strings |
| `"b"` | index `sum(n_bex)` | `n_bex` rows |

The totals MUST equal the part sizes: `32 x total n_trade = len(tx)`, `20 x total n_hash =
len(hash)`, total `n_pay = len(p)`, total `n_bex = len(b)`. A reader checks this.

### 3.8 Blocks and checkpoints

Each block is one market row's events in one segment, or all event-type-4 events of the segment
(block id `nmkt`):

    uv(n_ev)  uv(n_lv)  rc_stream_without_first_byte

`n_ev` and `n_lv` count every event and level of the block, **including its leading
checkpoints**. The stream is coded with local state for one market (3.12). Its parameters:

- `U` = the row's `U`, or `U = 1` for the event-type-4 block;
- start clock `ts0 = t0_ns / unit_ns`.

The first output byte of an LZMA-style range encoder is always 0, so it is not stored (3.10).

**Which blocks exist:**

- a block for every market row with **any** event in the segment. A trade or a tick-size change
  opens a block too, not only a book change;
- in the **first segment each writer process enters in the hour**, a block for every market whose
  held book is not empty: the full checkpoint. For a writer running through the hour boundary that
  is the hour's first segment. For one that starts mid-hour it is its first segment, when most
  books are still empty;
- an event-type-4 block, only when the segment has an event of type 4.

A market with no event in a segment has no block there. Its book is the end state of its last
block.

**Checkpoints.** Every market block starts with **exactly two** checkpoint events (type 5), one
per outcome, before any real event. Each is the full book, all levels, of the book the writer
holds just before the market's first event in the segment. Writers typically write outcome 0
then outcome 1; readers MUST accept either order. Every checkpoint level has size > 0. A
checkpoint can be **empty** (zero levels) when the writer holds no book yet. Checkpoints occur
only at the start of a block. The event-type-4 block has none.

**The checkpoint bitmap `ckpt`.** Bit `row` covers row `row`: byte `row >> 3`, bit `row & 7`, least
significant bit first. The bit is set exactly when the row has a block in the segment that starts
with checkpoints. It covers rows `[0, nmkt)`. The event-type-4 block is not in it. Writers emit
`ceil(nmkt / 8)` bytes before base64. Bits past the end of the bitmap read as 0.

**The full-book invariant.** A writer MUST NOT write a segment that breaks any of these:

- both outcomes have a checkpoint;
- no outcome has two;
- no checkpoint follows a real event;
- no checkpoint level has size 0;
- a token's first event in the block is a full book.

A reader MAY rely on the invariant (section 6). A reader SHOULD refuse a file whose market block
has its bitmap bit clear, or whose checkpoint follows a real event.

### 3.9 Side streams

**`tx`.** 32 raw bytes per type-2 trade, the venue's `transaction_hash`. In v1 they follow the
segment's arrival order of real events; in v2, block order (3.7).

**`payload.zst`.** One zstd frame holding the JSON `{"p": [...], "b": [...]}`:

- `"p"`: one string per type-3 and type-4 event, its payload. For type 3, the `tick_size_change`
  event JSON. For type 4, the event JSON (`best_bid_ask`, `new_market`, `market_resolved`, or an
  unknown event). Order: arrival order in v1, block order in v2.
- `"b"`: the **best-price exceptions**, one row
  `[event_index, entry_index, best_bid, best_ask]` per level whose delivered `best_bid` /
  `best_ask` differ from the book rebuilt after that entry. The two values are the delivered JSON
  strings (a reader takes a JSON `null` as empty and any other non-string as its JSON text).
  `entry_index` is the level's index within its event.
  - **v1:** `event_index` is the index in the decoded segment, i.e. `n_ck` + the real event's
    index in arrival order.
  - **v2:** `event_index` is the index **in the block**, checkpoints counted, as in the block's own
    decode. The rows are in block order.

  A row never names a checkpoint. On the capture the delivered values matched the rebuilt book for
  99.84 % of entries [capture]; the rest are stored here.

**`hash`** (optional, only when `has_hash` is true). The venue's 20-byte book hash: one item per
type-0 book event, and one per type-1 price_change entry, mirrors included. Order: arrival order of
real events in v1, block order in v2, entry order within an event. The hash depends on venue state
that is not in the messages and cannot be recomputed: 42 candidate serializations were tried
without a match. That is why it is stored at all. It is also why it is optional: about 40 B per
level change, roughly 12 times the rest of the file.

**A reader without the hash part.** When `has_hash` is false the `hash` part is absent and every
v2 `n_hash` is 0. Every event, level, clock, trade hash, payload and best exception still decodes
fully. Only the venue's book hash strings are unavailable; a dump leaves them empty. When
`has_hash` is true a reader MAY skip the part. Nothing else depends on it: in v2 the hash offsets
are their own prefix sums.

**`unsynced`.** A list of `[row, outcome]` naming the checkpoints of this segment that the writer
synthesized from a held book that had not yet received a venue snapshot. This happens when the
token was first seen mid-stream, or its connection resynced and no new snapshot had arrived. The
writer does not wait for the snapshot, because that would drop the token's changes until then.
Such a checkpoint may be empty. The venue's snapshot then follows as a type-0 event: a diff
against that book, which for an empty checkpoint is the full snapshot.

### 3.10 The range coder

A binary adaptive range coder in the style of LZMA. It has a 32-bit range, 11-bit probabilities of
a **0** bit, and a carry handled by a cache byte. The encoder flushes 5 bytes.

#### 3.10.1 Probabilities

With `shift` s > 0, every context is an 11-bit integer `p`, the probability of a 0 bit in 1/2048.
It starts at 1024. After coding bit b:

    b == 0:  p += (2048 - p) >> s
    b == 1:  p -= p >> s

With `shift 0` (the "two-rate mix"), every context holds two 15-bit counters `a` and `b`, both
starting at 16384:

    P = clamp((a + b) >> 5, 1, 2047)          # used as p
    bit 0:  a += (32768 - a) >> 4;   b += (32768 - b) >> 7
    bit 1:  a -= a >> 4;             b -= b >> 7

Every context of a block starts fresh at the start of the block. The order stream has its own
contexts and never uses shift 0 (3.6).

#### 3.10.2 Decoder

    init(stream):        # stream = the stored bytes, i.e. without the encoder's first (zero) byte
        range = 0xFFFFFFFF
        code  = the next 4 bytes, big-endian (byte 0 most significant)
    bit(p):
        bound = (range >> 11) * p
        if code < bound:  range = bound;                     b = 0
        else:             code -= bound; range -= bound;     b = 1
        update p with b (3.10.1)
        while range < 2^24:
            range <<= 8
            code = (code << 8) | next_byte()     # bytes past the end of the stream read as 0
        return b

A decoder can equally prepend the 0 byte and read 5 bytes at init.

A decoder MUST loop as shown: `while`, not a single step per bit. One step is enough while every
probability stays in [8, 2040], which holds for `shift >= 4`, where p stays in [15, 2033]. Under
`shift 0`, P can reach 4 or 2043, and one bit can need two steps; shifts 1-3 have the same
property. Vector `vec_renorm_shift0` has 23 such bits and decodes only with the loop.

#### 3.10.3 Encoder (for writers)

    init: low = 0 (64-bit), range = 0xFFFFFFFF, cache = 0, cache_size = 1
    bit(p, b):
        bound = (range >> 11) * p
        if b == 0: range = bound
        else:      low += bound; range -= bound
        update p with b
        while range < 2^24: range <<= 8; shift_low()
    shift_low():
        if (low & 0xFFFFFFFF) < 0xFF000000 or (low >> 32) != 0:
            carry = low >> 32; temp = cache
            repeat: output(temp + carry); temp = 0xFF; cache_size -= 1; until cache_size == 0
            cache = (low >> 24) & 0xFF
        cache_size += 1
        low = (low & 0x00FFFFFF) << 8
    flush: shift_low() 5 times

The first output byte is always 0, and a writer MUST drop it. A block, or the order stream, is
stored as the output without its first byte.

#### 3.10.4 Adaptive Elias gamma

A "uint context" is one gamma table: unary contexts `exp[0..63]` and low-bit contexts
`mant[n][j]` for `n` in 1..63 and `j` in `0..n-1`. All start fresh with the block. To code a value
`v >= 0`:

    x = v + 1;  n = bitlen(x) - 1
    n one-bits, the i-th with context exp[i];
    then a zero-bit with context exp[n], omitted when n == 63;
    then the n low bits of x, most significant first, each with its own context mant[n][position]

Decoding reads one-bits until a zero (or 63 ones), then n bits: `x = 1`, then for each bit
`x = 2x + bit`, and `v = x - 1`.

A signed value is coded as `gamma(zz(v))`. That is "zigzag gamma" below.

`mant[n][j]` may be indexed by bit significance or by reading position: the two are the same set
of contexts under a different labelling, so the coded bits are the same.

### 3.11 Flags

| bit | name | effect when set | production |
|---|---|---|---|
| 1 | NoSizeDelta | no size flag bit: every size is absolute | off |
| 2 | NoPriceDelta | a price_change price's reference is 0 instead of the token's last price | off |
| 4 | NoPrevCtx | the "previous value" contexts stay 0: previous event type, cont, same bit, outcome, price_change side, dp bucket, trade side, dts bucket | off |
| 8 | NoCaches | no size caches: no hit bits, only literals | off |
| 16 | LatDelta | `lat` is coded as a delta from the previous `lat` of the same event type | **on** |
| 32 | NoDtsBucket | the dts bucket stays 0 | off |
| 64 | PriceBest | a price_change price is coded as a distance from the token's current best on its side | **on** |

`flags 80` = 16 + 64. The conformance vectors use `flags 80` and `flags 0`; bits 1, 2, 4, 8 and 32
are defined for completeness and exercised by no vector. A reader that does not implement a bit
MUST refuse a file that sets it.

### 3.12 Block decoding (normative)

#### 3.12.1 State, reset at the start of every block

The block's market is local market 0, with tokens 0 and 1 (= outcomes 0 and 1).

| state | initial |
|---|---|
| `prev_et`, `prev_cont`, market `prev_out` | 0 |
| `prev_ts` | `ts0 = t0_ns / unit_ns` |
| `dts_bucket` | 0 |
| `prev_lat[et]` for et 0..3 | 0 |
| per token: `prev_pc_side`, `dp_bucket`, `last_px`, `last_trade_px`, `prev_trade_side` | 0 |
| per token and side: book (price -> size, sizes 0 = absent), best (none) | empty |
| per token: delta cache and absolute cache, 8 entries each | empty |
| every probability context | fresh (3.10.1) |

The **best** bid is the highest bid price with size > 0, and the best ask the lowest ask with
size > 0. Both are maintained over every level written in the block: book, checkpoint,
price_change and mirror levels. When the best level is removed, the new best is found by scanning.

#### 3.12.2 Contexts of a block

Bit contexts:

| name | index |
|---|---|
| `ET` | [prev_et 0..5][tree node 1..7] |
| `CONT` | [prev_cont] |
| `OUT` | [et 0..5][market prev_out] - type-1 entries use et 1 |
| `MIR` | [entry index == 1 ? A : B] (two models) |
| `PSIDE` | [outcome][token's prev_pc_side] |
| `TSIDE` | [token][token's prev_trade_side] |
| `SF` | [ki][side] (size flag) |
| `DCH`, `ACH` | [ki][side] (delta / absolute cache hit) |

Uint (gamma) contexts:

| name | index |
|---|---|
| `DTS` | [et 0..4][dts_bucket 0..2] |
| `LAT` | [et 0..3] |
| `NB`, `NA` | [kind] - kind 0 = type 0, kind 1 = type 5 |
| `DPB` | [kind][side][first?] |
| `M` | one (price_change entry count) |
| `DPP` | [side][token's dp_bucket] |
| `TDP`, `TSZ`, `FEE` | one each, shared by both tokens and both sides |
| `DCI`, `ACI` | [ki] - **no side** |
| `SZD`, `SZA` | [ki][side] |

`ki` selects the size model set: 0 = price_change entry, 1 = type-0 level, 2 = type-5 level. Each
set has its own flag, hit, index and literal models.

#### 3.12.3 Per event

    # event type: a 3-bit LZMA bit tree, most significant bit first
    node = 1; repeat 3 times: node = 2*node + bit(ET[prev_et][node])
    et = node - 8;  et > 5 is corrupt;  prev_et = et (unless flag 4)

    # receive clock: every type except 5
    if et != 5:
        u = gamma(DTS[et][dts_bucket]);  ts = prev_ts + unzz(u);  prev_ts = ts
        unless flag 4 or flag 32: dts_bucket = (u == 0) ? 0 : (u < 1024) ? 1 : 2   # of the CODED value u
    else: ts = 0                                   # a checkpoint has no clock; prev_ts untouched

    # latency: types 0..3
    if et <= 3:
        v = unzz(gamma(LAT[et]))
        lat = flag 16 ? prev_lat[et] + v : v;  prev_lat[et] = lat
    else: lat = 0

    if et == 0: cont = bit(CONT[prev_cont]); prev_cont = cont (unless flag 4)   else cont = 0
    if et in {0, 2, 3, 5}: out = bit(OUT[et][prev_out]); prev_out = out (unless flag 4)   else out = 0

    then by type:  0 or 5 -> BOOK(kind = 0 or 1);  1 -> PRICE_CHANGE;  2 -> TRADE;  3, 4 -> nothing more

The dts bucket comes from the zigzag value, so a |dts| between 512 and 1023 is bucket 2. Vector
`vec_dts_bucket` decodes only under this reading. `prev_ts` and `dts_bucket` are block-wide, over
every event type.

**BOOK** (type 0 book diff, type 5 checkpoint), for token `tok = out`:

    nb = gamma(NB[kind]);  na = gamma(NA[kind])
    for side in (0 = bids, 1 = asks), count in (nb, na):
        for k in 0..count-1:
            dp = gamma(DPB[kind][side][k == 0])
            p  = (k == 0) ? dp : prev_p + dp + 1;  prev_p = p;  p > U is corrupt
            z  = SIZE(ki = kind + 1, tok, side, p)
            set book[tok][side][p] = z;  emit level (out, side, p, z, mirror 0)

Prices ascend strictly within each side. A type-0 event lists only the levels that differ from the
held book, and size 0 removes a level. A type-5 event lists every level of the token's book.

**PRICE_CHANGE** (type 1):

    n = gamma(M);  prev_explicit = false
    for k in 0..n-1:
        if prev_explicit (never for k == 0):
            if bit(MIR[k == 1 ? A : B]) == 1:          # mirror of the previous entry e
                level = (1 - e.out, 1 - e.side, U - e.p, e.z), mirror 1
                set it in the book; update last_px (below); prev_explicit = false; emit; continue
        out  = bit(OUT[1][prev_out]);  prev_out = out (unless flag 4)
        tok  = out
        side = bit(PSIDE[out][prev_pc_side[tok]]);  prev_pc_side[tok] = side (unless flag 4)
        d    = unzz(gamma(DPP[side][dp_bucket[tok]]))
        if flag 64 and the token has a level on that side:
            p = (side == 0) ? best_bid[tok] - d : best_ask[tok] + d      # coded: zz(best - p) / zz(p - best)
        else:
            ref = flag 2 ? 0 : last_px[tok];  p = ref + d                # coded: zz(p - ref)
        dp_bucket[tok] = (|d| == 0) ? 0 : (|d| <= 2) ? 1 : 2  (unless flag 4)
        p < 0 or p > U is corrupt
        z = SIZE(ki = 0, tok, side, p);  set book[tok][side][p] = z
        update last_px;  prev_explicit = true;  emit level (out, side, p, z, mirror 0)

    update last_px after EVERY entry, explicit or mirror, at token t and price p:
        last_px[t] = p;  last_px[1 - t] = U - p

A mirror codes nothing but its bit. It does not touch `prev_out`, `prev_pc_side`, `dp_bucket` or
the size caches. When flag 64 is set and the side is empty, the price falls back to the delta from
the token's last price.

**TRADE** (type 2), for token `tok = out`:

    side = bit(TSIDE[tok][prev_trade_side[tok]]);  prev_trade_side[tok] = side (unless flag 4)
    p = last_trade_px[tok] + unzz(gamma(TDP));  last_trade_px[tok] = p
    z = gamma(TSZ)          # trade size, in trade_size_unit_micro
    fee = gamma(FEE)        # fee_rate_bps
    emit level (out, side, p, z, mirror 0)       # a trade does NOT change the book

**SIZE(ki, tok, side, p)**, where `prev = book[tok][side][p]` and 0 means absent:

    path = (prev != 0 and not flag 1) ? bit(SF[ki][side]) : 0      # 1 = delta, 0 = absolute
    if path == 1:
        if not flag 8:
            if bit(DCH[ki][side]) == 1:                # coded even when the cache is empty
                i = gamma(DCI[ki]);  d = dcache[tok][i];  move entry i to the front
            else:
                d = unzz(gamma(SZD[ki][side]));  insert d at the front (entry 7 drops)
        else: d = unzz(gamma(SZD[ki][side]))
        return prev + d
    else:
        if not flag 8:
            if bit(ACH[ki][side]) == 1:
                i = gamma(ACI[ki]);  z = acache[tok][i];  move entry i to the front
            else:
                z = gamma(SZA[ki][side]);  insert z at the front (entry 7 drops)    # plain, NOT zigzag
        else: z = gamma(SZA[ki][side])
        return z

The rules that keep a decoder in step with the stream:

- The delta path updates **only** the delta cache, and the absolute path **only** the absolute
  cache. Vector `vec_size_cache` decodes only under this reading.
- "Move to front" at index k shifts entries 0..k-1 down one.
- "Insert at the front" shifts entries 0..6 down one and drops entry 7. It does not search for the
  value.
- A resulting size below 0, or a cache index past the filled entries, is corrupt.

The encoder's choice, which a decoder does not need: use the delta path if the delta is in the
delta cache; else the absolute path if the size is in the absolute cache; else the shorter
literal, delta when `bitlen(zz(delta) + 1) < bitlen(size + 1)`. A writer following it never codes
a literal for a value already in the cache, so a decoder that moves an equal entry to the front
instead of inserting a duplicate decodes the same on every such file.

#### 3.12.4 End of a block

After `n_ev` events the decoded level count MUST equal the block's `n_lv`. A valid stream is never
read past its stored end: 0 bytes over every conformance vector. Readers read missing bytes as 0,
so that a corrupt stream decodes to garbage instead of failing mid-bit, and then reject it. A
reader MUST reject a stream read more than 8 bytes past its end, and MAY use a lower limit of at
least 4 (the reference readers use 4 and 8). Every limit in that range accepts every valid stream.

### 3.13 Units and conversions

| quantity | formula |
|---|---|
| token id of a level | `markets[row].tokens[level.out]` |
| price (micro) | `Lp x price_unit_micro` (all event types) |
| size (micro shares) | `Lz x size_unit_micro` for types 0, 1, 5; `Lz x trade_size_unit_micro` for type 2 |
| receive time (ns) | `ts x unit_ns` |
| receive time (us), as exported | `floor(ts x unit_ns / 1000)` |
| venue time (ms) | `floor(ts x unit_ns / 1e6) - lat` |

The writer stores `ts = floor(receive_ns / unit_ns)` and
`lat = floor(ts x unit_ns / 1e6) - venue_ms`, so the venue's millisecond timestamp comes back
exactly. Type 4 has `lat = 0`: its venue time is the receive time in ms. A checkpoint has no clock
(`ts = 0`, `lat = 0`).

---

## 4. Events

### 4.1 Event types

| et | name | in which block | coded fields | levels |
|---|---|---|---|---|
| 0 | book (diff) | market | dts, lat, cont, outcome | the snapshot's differences from the held book; size 0 removes a level |
| 1 | price_change | market | dts, lat; per entry outcome, side, price, size, or a mirror bit | its entries in delivered order |
| 2 | trade | market | dts, lat, outcome, side, price, size, fee | 1 (outcome, side, price, size) |
| 3 | tick_size_change | market | dts, lat, outcome | none; payload in `"p"` |
| 4 | other | the type-4 block (id `nmkt`) | dts | none; payload in `"p"` |
| 5 | checkpoint | market, leading only | outcome | the token's full book |

Events in a type-4 block are all type 4, and type 4 occurs only there; a reader SHOULD refuse a
file that breaks either rule. A type-0 event comes from the venue's `book` message, which is a full
snapshot, and is stored as the difference from the book the writer holds. Applied to that book it
gives the snapshot exactly.

### 4.2 Fields

| field | events | meaning |
|---|---|---|
| `et` | all | event type, 0..5 |
| `out` | 0, 2, 3, 5 | outcome: the token is `tokens[out]` of the block's row |
| `cont` | 0 | 1 when the book event is not the first event object of its websocket frame. The object before it need not be a book |
| `ts` | 0..4 | receive time in `unit_ns` |
| `lat` | 0..3 | `floor(receive ms) - venue ms` |
| `fee` | 2 | `fee_rate_bps` as the venue states it |
| level `out`, `side` | levels | outcome; side 0 = BUY / bid, 1 = SELL / ask (for a trade: the venue's `side`) |
| level `Lp`, `Lz` | levels | price and size in the row's units |
| level `mirror` | type-1 entries | 1 = this entry was not coded; it is the mirror of the previous explicit entry |
| tx hash | 2 | from `tx` |
| payload | 3, 4 | from `"p"` |
| book hash | type 0, type-1 entries | from `hash`, when `has_hash` |
| best_bid, best_ask | type-1 entries (any real level) | from `"b"`, only where they differ from the rebuilt book |

### 4.3 The mirror flag

Polymarket's two outcome books reflect each other: a bid at p on one token is an ask at 1 - p on
the other. A price_change event usually carries both entries; on the capture 869,811 of 1,739,622
entries were exact mirrors [capture]. An entry equal to the mirror of the previous explicit entry
of the same event is stored as a single bit. A mirror means: other outcome, flipped side, price
`U - p`, same size. Decoded, it is a full level with `mirror = 1`. It changes the book like any
entry, and it has its own book hash and can have a best exception.

### 4.4 The two clocks

- **Receive clock** (`ts`): the writer's wall time when it finished reading the frame from the
  socket, after TLS and websocket de-framing and before parsing or queueing. Every event of one
  frame has the same receive time. It is stored at 1 microsecond resolution (`unit_ns` 1000). Its
  accuracy against UTC is the host clock's, which the publisher of a file states separately.
  Within a block, events are in arrival order and `ts` can step back slightly (`dts` is signed).
- **Venue clock**: the venue's `timestamp` field, in milliseconds, stored as `lat`, its difference
  from the receive clock in ms.

Joins between the book and the trades should use the venue clock. The two arrive on different
websocket channels, so on the receive clock a trade can land before the update that caused it.

### 4.5 What the writer records

The writer records at the parsed-event point, before any throttle, depth limit or row cap, and
after removing exact duplicates delivered by redundant connections:

- every price_change entry, including ones that do not change the book;
- every book snapshot, as a diff against the held book;
- every trade, with `fee_rate_bps` and `transaction_hash`;
- every tick_size_change, as type 3 with the event JSON as payload;
- every best_bid_ask, new_market, market_resolved or unknown event, as type 4 with the payload.

It does not record PONG replies, non-JSON frames, or fields of unknown tokens. A price above 1.0
is dropped and counted.

### 4.6 What is dropped, and why it is still lossless

- `best_bid` and `best_ask` are derivable from the book, and are stored only where they differ
  (`"b"`).
- The mirror entry is a flag.
- Market and asset id strings live in the market table.
- Connection ids, sequence numbers and JSON formatting are dropped.

The venue's book hashes are not derivable, so they are kept in the optional `hash` part.

**Reproducing the delivered best_bid / best_ask.** After each price_change entry the writer prints
the token's rebuilt best bid and best ask from its held book. It compares those strings with the
delivered ones, and writes a `"b"` row exactly when either differs.

The printing is the minimal decimal of the 1e-4 tick: the integer part, then the fractional digits
with trailing zeros removed, and no trailing point. Examples: `0.52`, `0.001`, `0.0005`, `1`. An
empty bid side prints `0`, and an empty ask side prints `1`.

So for an entry without a `"b"` row, a reader reproduces the venue's strings exactly from the
rebuilt book. An entry whose message had no best_bid / best_ask fields gets no row.

---

## 5. Reading

### 5.1 Full decode of a segment

1. Decode `order`: n block ids, n = `n_ev`.
2. Decode every block in `bidx` order, each with its own fresh state (3.12), `U` of its row (1 for
   id `nmkt`) and `ts0 = t0_ns / unit_ns`.
3. The **reference order** of the segment (the order of `events.csv`): first every block's leading
   checkpoints, block by block in ascending row; then the real events, taking for each id of the
   order stream the next unread event of that block.
4. Check that each block's real events are used exactly. Check the counts: decoded checkpoints =
   `n_ck`, real events = `n_ev`, real levels = `n_lv`.
5. Attach the side streams:
   - **v1:** walk the real events in reference order. The k-th trade gets the k-th 32 bytes of
     `tx`. The k-th type-3/4 event gets `"p"[k]`. Each type-0 event takes the next hash item, and
     each type-1 event takes one per entry. Then each `"b"` row names (event index in the decoded
     segment, entry).
   - **v2:** do the same per block, in block order over that block's events with checkpoints
     skipped, from the block's run (3.7). Each `"b"` row names (event index in the block, entry).
     Map (block, event in block) to the reference order.

   The streams MUST be used exactly, with nothing left over.

A file is decoded segment by segment, in header order.

### 5.2 Reading one market by range

A reader with only ranged reads (HTTP Range, object storage) reads one market as follows:

1. Read bytes `[0, 8)` for the magic and `header_len`, then the header JSON. Check the version.
2. Read `markets_zst`, decompress it, and find the market's rows by `slug` or `condition_id`. A
   unit change gives a market several rows (3.4).
3. For each segment and each of the market's rows with `row < nmkt` and its `ckpt` bit set: read
   `bidx`, or only a prefix of it up to the row, since entries are in ascending row. Walk it to the
   row's entry to get the block's offset and length. In v2 also get the six counts and their
   prefix sums.
4. Read the block bytes from `blocks` and decode them alone (3.12).
5. **v2:** read the block's `tx` run (`32 x n_trade` bytes) and its `hash` run (`20 x n_hash`
   bytes, only if wanted). Read the segment's `payload.zst` only if `n_pay > 0` or `n_bex > 0`,
   and take the block's slices of `"p"` and `"b"`. The block's events are then complete.
   **v1:** the side streams follow the segment's global order. A v1 reader must therefore either
   decode every block of the segment plus the order stream, or use the `.evidx` sidecar
   (Appendix A). Without either, the levels, sides, prices, sizes, clocks, trade sizes and fees of
   a single-market read are exact, but trades have no tx hash and type-3 events no payload.

Measured on the production hour (a v1 file): one market's rows fetched **24.6 MB** of the
533.5 MB by HTTP range, in **0.25 s**. A real hour's market table is about 70 MB of JSON for about
175k rows before compression; it is the largest fixed cost of a one-market read.

### 5.3 The replay rule

The book of a token is a map from (side, price) to size. Apply a block's events in stored order:

- **type 5 (checkpoint):** replace the token's book with the checkpoint's levels. An empty
  checkpoint empties the book.
- **type 0 (book diff):** set each listed level to its size; size 0 removes the level.
- **type 1 (price_change):** set each entry's level (mirrors included) to its size; size 0 removes
  it.
- **type 2 (trade):** the book does not change.
- **types 3 and 4:** the book does not change.

Inside the decoder this is what `set book[...]` does (3.12.3). The block decoder's book after any
event is the token's book at that event.

### 5.4 The book at time T

1. Let `Tq = floor(T / unit_ns)`.
2. Take the **latest segment with `t0_ns <= T`** whose `ckpt` bitmap has a row holding the token,
   that is a set bit for a row whose `tokens` contain it. A segment has at most one such row.
3. Decode only that block. Apply its checkpoints, then its events in stored order while `ts <= Tq`,
   and stop at the first event with `ts > Tq`.
4. If no segment qualifies, the token has no book in this file. `books.csv` writes this as
   `token_id,ts_ns,NONE,,`.

Step 2 is O(1) per segment through the bitmap. A market with no block in the segment containing T
did not change since its last block, so the end state of that block is its book at T.

### 5.5 Starting at any 5-minute boundary

Let t0 be any segment start in the file:

- if the market has a block in the segment starting at t0, that block opens with both tokens' full
  books, and replay starts there;
- if it has none, its book at t0 is the end state of its latest earlier block in the same file.

There always is one if the book is not empty. In the first segment each writer process enters in
the hour, every non-empty held book gets a block (3.8), and after that any change to the book is
an event, which opens a block. Either way, no more than one block is decoded per segment.

### 5.6 Late events

The writer stores an event in segment **max(the segment of its receive time, the writer's current
segment, the lowest segment still open)**. An event therefore sits in a later segment than its own
`ts` in two cases:

- its frame was processed after its segment had closed (2 s after the segment's end);
- a frame from a redundant connection had already moved the writer into the next segment.

Such an event keeps its own, earlier `ts`, so in general its `dts` is negative. An event is never
stored in a segment earlier than its own: every event of a segment has `ts < t0_ns + 300 s`.

The book-at rule (5.4) uses stored order:

- A query at a T in the earlier segment does not see a late event stored in a later segment, even
  though its `ts <= T`.
- Within a block, the walk stops at the first event with `ts > Tq`. A later event with an earlier
  `ts` is not applied.

So "the book at T" means the book as recorded in arrival order, cut at the first event after T.
A reader MUST NOT re-sort by `ts`.

### 5.7 Measured reading

On the production hour [benchmark]: a full decode with every check, events and levels in the
reference order, takes 49.3 s on one core and 10.8 s on eight in the Rust reader, and 92.8 s on
one core in a Go reader. One market's blocks decode in about 5 ms once the file is open.

---

## 6. Invariants

A writer MUST keep these. A reader MAY rely on them, and SHOULD check the ones a file alone can
show. The last column says how the conformance vectors test each one.

| # | invariant | checkable from the file | vectors |
|---|---|---|---|
| I1 | Magic `EVM4`; `version` is `evmin/4` or `evmin/5`; `family rc`, `layout mkt` | yes | all; a patched `evmin/6` header must be refused |
| I2 | Every offset/length lies inside the body; `bidx` lengths sum to the `blocks` part | yes | all |
| I3 | `bidx` rows strictly ascend, each `<= nmkt`; at most one block per row | yes | all |
| I4 | A market block exists for a row **iff** its `ckpt` bit is set | yes | all |
| I5 | Every market block starts with exactly two checkpoints, one per outcome, in either order; no checkpoint after a real event; every checkpoint level has size > 0 | yes | all |
| I6 | A block exists for every market with any event in the segment (book change, trade or tick-size change), and, in the first segment each writer process enters in the hour, for every non-empty held book | no | the books of `*.books.csv` |
| I7 | Type 4 only in block `nmkt`, which holds only type 4 and no checkpoints | yes | `basic_*` (type-4 events in every segment) |
| I8 | Counts: order `uv(n)` = `n_ev`; decoded checkpoints = `n_ck`; real levels = `n_lv`; each block's levels = its `uv(n_lv)`; every block event appears in the order stream exactly once | yes | all |
| I9 | v2: the bidx counts equal each block's decode; totals equal the part sizes (3.7); `n_hash = 0` without `has_hash` | yes | `basic_v2` |
| I10 | Side streams are used exactly: 32 B per trade, one `"p"` per type-3/4 event, 20 B per type-0 event and per type-1 entry when `has_hash`; `"b"` rows name an existing level of a real event | yes | `basic_*` (`has_hash` true); `vec_*` (false) |
| I11 | Units: every block's values lie on its row's grid; `U = 1e6 / price_unit_micro`; prices in `[0, U]`; a row's units never change; a market has at most one row per segment; a finer unit opens a new row | price range: yes | `basic_*` (a mid-hour unit split); `units_hour_a/b` (units differ between hours) |
| I12 | Book and checkpoint levels: bids then asks, strictly ascending price within a side | by construction (dp >= 0) | all |
| I13 | A mirror never starts an event and always follows an explicit entry | by construction | all |
| I14 | `t0_ns` is a multiple of 300 s, segments ascend, all inside the file's hour | yes | all |
| I15 | Deterministic bytes: the same input to the same writer gives the same file | no (a writer property) | - |
| I16 | No header field depends on the time of writing | no | - |

**On I15.** The bytes of the `markets_zst` and `payload.zst` parts depend on the zstd
implementation and level. Determinism is a property of one writer version: a different conforming
writer produces different but equally valid bytes. A file's digest therefore identifies one
delivery of an hour, not its content in the abstract; the core dump (8.2) identifies the content.

**Hour start and files per hour.**

- `hour_start_ns` is always the file's UTC hour start, a multiple of 3600 s. The first segment's
  `t0_ns` is >= it, and equal only when the writer had events in the hour's first 5 minutes.
- A writer process writes one file per hour, unless an encode or disk failure closed the hour
  early; it then continues the hour in a second file, with the later segments only.
- Files of different writer processes (a restart, or two writers in parallel) can cover
  overlapping times. Each file is complete and self-contained. Merging them is outside this
  format.

---

## 7. Design rationale

"Core" means the coded blocks plus the order stream, without side streams. "B/chg" means bytes per
level change.

### 7.1 An event log, not snapshots

**Decision.** Store every change as an event, and put a full book only at block starts.

**Alternatives.**

- **A snapshot per change.** A full book costs about 212 B per token at 300 s [capture], against
  3.31 B per change in the chosen layout [capture]. That is about 64 times more per change.
- **Snapshots on a grid**, for example at most one row per token per 100 ms with three levels. It
  loses every state between samples and cannot answer "every change".
- **A checkpoint for every token every 5 minutes.** At full-venue scale this is about 23 GB a day,
  more than the events. Checkpoints written only for markets that changed, plus one full set per
  hour file, cost about 1.9 GB a day [writer].

**Why it won.** It is lossless, and the cost follows the market's activity. Checkpoints bound how
far back a reader has to go.

### 7.2 One block per market per 5-minute segment

**Decision.** `layout mkt`: each segment holds one independently decodable block per market that
changed, plus an order stream that restores the arrival order across markets.

**Alternatives.**

- **One block per segment in arrival order** (`layout time`). It is smaller: 3.06 B/chg core
  against 3.31 at 300 s, flags 80, shift 4 [capture]. But the book of one market then needs the
  whole segment decoded.
- **Shorter or longer segments.** Under the chosen coding, core size is 3.436 / 3.310 / 3.289 B/chg
  at 60 / 300 / 900 s [capture]. Book-at seek time: 6.9 / 134 ms mean, 44 / 311 ms p95, at
  60 / 300 s [capture]. Checkpoint bytes: 106,084 / 20,780 / 7,134 [capture]. The size gain
  flattens after 5 minutes, while seek time grows with the window.

**Cost of the choice, measured.**

- The order stream: 1.08 B/chg [all-markets sample], 15.9 % of the production hour.
- Per-block model warm-up: quiet markets start their models from p = 0.5. Over 20,546 market
  blocks per 10 minutes the blocks cost 5.20 B/chg [all-markets sample], against 3.3 on the
  capture's 20 busy markets.

**Why it won.** Reading one market by range (5.2) is the main access path, and blocks are what
make decoding parallel: an hour is about 943 k independent blocks [production hour]. On the
production hour one market read 24.6 MB of 533.5 MB in 0.25 s.

### 7.3 Range coding with context models

**Decision.** Code each block's symbols with an adaptive binary range coder (LZMA style), using
contexts chosen per field (3.12).

**What was measured.**

- **Modelling, not the entropy coder, makes the size.** With every modelling step off (no mirror
  fold, no size delta, no price model, no contexts, no caches) the core is 7.906 B/chg [capture].
  The steps from a 3.782 baseline (shift 5) [capture]:

  | step | effect |
  |---|---|
  | mirror fold | -27 % (5.169 without it) |
  | size delta | -20 % (4.728 without it) |
  | size caches | -2.4 % (3.875 without them) |
  | price as distance from the best (flag 64) | -6.4 % (3.539) |
  | lat as delta (flag 16) | -5.4 % (3.577) |
  | shift 4 | -1.1 % (3.739) |
  | chosen: flags 80, shift 4 | **3.310** |
  | previous-value contexts (flag 4), dts bucket (flag 32) | about 0 (3.777 / 3.776 without them) |
  | two-rate mix (shift 0) | a further -0.4 % (3.296), not adopted |


**Trade-off, stated honestly.**

- The large gains come from modelling the venue: mirrors, deltas from the held book, the best
  price. That modelling needs a book-aware decoder, whichever entropy coder is used.
- The range coder gives up in-place reads and some decode speed: it is bit-serial. v3 (below) keeps
  the models and moves to static multi-symbol coding for speed.
- In return: one walker serves as both encoder and decoder, so they cannot drift. Each small block
  is self-contained with no frame or dictionary overhead. Encoding is cheap enough to run live:
  about 0.0067 cores per 1k changes/s, about 0.11 cores at production rates [writer].

**Why it won.** Smallest of the configurations tested, at a cost a live writer can pay, with one
code path. A future version can put the same modelled symbols under a faster entropy coder; that
is the plan of the next version (section 9).

### 7.4 Both clocks, and their units

**Decision.** Store the receive time in microseconds as a delta (`dts`), and the venue's
millisecond timestamp as `lat`, coded as a delta from the previous `lat` of the same event type.

**Alternatives.**

- **Only the venue clock.** It loses arrival order across channels and the writer's latency.
  Arrival order matters because a trade can arrive before the book update that caused it.
- **Only the receive clock.** It loses the venue's own time, which joins need.
- **ns receive clock.** 3.789 B/chg core [capture], +14 % over µs.
- **ms receive clock.** 2.812 B/chg core [capture], 15 % smaller than the 3.310 of µs.

**Why µs.** The clocks are a third of the file: `dts` 710,238 B plus `lat` 309,627 B of 2,891,849
core [capture]. Microseconds keep the order and spacing of messages parsed within one
millisecond. Nanoseconds would record digits the host clock cannot give. Coding `lat` as a delta
saves 5.4 % [capture].

### 7.5 A JSON header

**Decision.** Store the header and the market table as JSON, the table zstd-compressed.

**Alternative.** A binary metadata header.

**Why JSON won.**

- The header is read once per file and is small next to the body.
- It is readable with any language's standard library, and it makes the file self-describing
  (goal 5).
- New keys can be added within a version (2.2 rule 4).

**The cost.**

- The market table is large: about 120 B per row, about 70 MB of JSON for about 175k rows on a
  production hour. A general JSON parser takes about 0.3 s over it; a dedicated scanner is faster.
- The table has to be read before any one-market read (5.2).

### 7.6 Checkpoints every 5 minutes

**Decision.** Each segment's block of a market opens with both tokens' full books. A market gets
such a block in every segment where it had any event (a book change, a trade or a tick-size
change). Every non-empty held book gets one in the first segment each writer process enters in
the hour.

**Alternatives.**

- **No in-band checkpoints**, with a replay from the hour start. Seek time grows with the hour.
- **Checkpoints for every token in every segment.** About 23 GB a day at full-venue scale [writer].
- **60 s segments.** Checkpoints become 3.5 % of the file [capture].

**Why it won.**

- Any 5-minute boundary is a valid start, with one block to decode.
- Every block decodes alone. That is what makes per-market reads and parallel decoding possible.
- On the capture, 100 of 100 random (token, time) books matched a replay of the raw JSON at
  every interval tested [capture]. The same held on the all-markets sample.

**Unsynced checkpoints** are written from the held book rather than waiting for the venue's
snapshot. Waiting would drop the token's changes until the snapshot came. The `unsynced` list says
which checkpoints those are.

### 7.7 Side streams outside the blocks, and why v2 reordered them

**Decision.** Keep transaction hashes, payload strings, best-price exceptions and book hashes out
of the range-coded blocks, in their own parts.

**Why.** They are not modelled.

- Transaction hashes are 32 incompressible bytes: 192,640 B for 6,020 trades, 0.22 B/chg
  [capture].
- Book hashes are 20 incompressible bytes per entry.
- Payloads are JSON text, which zstd handles: payloads and exceptions are 9,921 B at 300 s
  [capture].

Outside the blocks they cost their raw size. A reader can skip them, and the hash part, the
largest by far, can be optional (3.9).

**Alternative.** Store them inside the blocks. A reader would then have to decode a block to skip
its hashes, and `has_hash` would change the block bytes.

**Why v2 reordered them.** In v1 the side streams follow the segment's global arrival order. The
position of one market's k-th trade hash depends on every other market's events, so a one-market
read cannot attach tx hashes or payloads without decoding the whole segment. v2 puts the streams
in block order and adds each block's counts to `bidx`, so a block's runs are prefix sums.

**Measured.** Every one of 18,682 blocks of the all-markets sample decoded alone matched the full
decode. The file shrank from 17.65 MB (v1) to 13.71 MB (v2), because the block-ordered exception
rows compress better. The `.evidx` sidecar gives v1 files the same positions without rewriting
them (Appendix A).

### 7.8 One file per hour

**Decision.** One file per UTC hour, written once after the hour's last segment.

**Why.**

- Closed hours are the unit of completeness: a file is never appended to, and a correction is a
  new file for the whole hour.
- It bounds the writer's memory and its loss: a crash loses at most the open hour.
- The market table and the full checkpoint are paid once per file. At production scale the table
  is about 0.7 GB a day, and the hour's full checkpoint 0.4-2.5 GB a day [writer].

**Alternatives.**

- **A file per day.** It saves 23 of 24 tables and full checkpoints, but delivers a day late and
  loses a day on a crash.
- **A file per segment.** It multiplies the table and the full checkpoint by 12.

### 7.9 Units per file and per row

**Decision.** Store prices and sizes as integers on the coarsest grid that holds the market's
values (3.4), with `U` units in 1.0.

**Why.**

- Integers on the market's own grid code in fewer bits than a global finest grid.
- With `U` exact, the mirror price is the integer `U - p`.
- Fixing a row's units when it is created, and opening a new row for a finer value, means nothing
  already written is ever re-interpreted.

**Alternative.** Fixed global units (1e-4 price, 1e-6 shares). This is simpler, but every value
gets larger. The saving of the gcd grid was not measured separately.

### 7.10 Deterministic bytes and an optional hash

- **Determinism (I15).** With it, a file's digest identifies a delivery, so the same hour
  delivered twice is applied once. It is also what an encode-twice test of a writer checks.
- **Optional `hash` (3.9).** The venue's hashes cannot be recomputed, so they must be stored or
  lost. At about 12 times the rest of the file they are kept for archives and left out of
  distributed files. No other byte depends on them.

---

## 8. Conformance

### 8.1 Conformance vectors

The vectors are in `tests/golden/` of this repository. They were written by the production
writer's code, and three independent readers reproduce them byte for byte: the writer's own
decoder, a Go reader written from this specification alone, and the Rust reader of this
repository.

| file | version | shift / flags / has_hash | what it covers |
|---|---|---|---|
| `basic_v1.probe` | v1 | 4 / 80 / true | 3 segments, 5 market rows including a mid-hour unit split, type-4 events, unsynced and empty checkpoints, best exceptions (also on mirrors), hashes. 384 events, 1,927 levels |
| `basic_v2.probe` | v2 | 4 / 80 / true | the same stream as `basic_v1.probe`, written as v2 |
| `units_hour_a.probe`, `units_hour_b.probe` | v1 | 4 / 80 / false | one market whose units differ between two hour files |
| `vec_dts_bucket.probe` | v1 | 4 / 80 / false | the dts bucket is of the coded value, not of the absolute dts |
| `vec_size_cache.probe` | v1 | 4 / 80 / false | the delta cache is not fed on the absolute path |
| `vec_flags0.probe` | v1 | 4 / 0 / false | lat absolute; price as a delta from the token's last price |
| `vec_shift5.probe` | v1 | 5 / 80 / false | shift 5, in blocks and in the order stream |
| `vec_shift0.probe` | v1 | 0 / 80 / false | the two-rate mix in blocks; the order stream at shift 5 |
| `vec_renorm_shift0.probe` | v1 | 0 / 80 / false | 23 shift-0 bits that need two renormalisation steps (3.10.2); its books include a `NONE` row and an empty book |
| `basic_v1.probe.evidx` | evidx/1 | - | the `.evidx` sidecar of `basic_v1.probe` (Appendix A) |

Each `vec_*` vector decodes differently under the wrong reading it is named for; `basic_*` alone
does not separate the dts-bucket or size-cache readings.

### 8.2 What a conforming reader MUST reproduce

1. For each vector, its **full dump** MUST equal the matching `events.csv` byte for byte:
   `basic_v1.probe` and `basic_v2.probe` give `basic.events.csv`; `X.probe` gives
   `X.events.csv`.
2. For `basic_*` and every `vec_*`, the **books** at the queries of `*.books_queries.csv` MUST
   equal `*.books.csv`. That is 12 of 12 queries for `basic_*`.
3. A conforming reader MUST refuse an unknown version (2.2).

A reader of the core alone (events and levels without the side streams) MUST reproduce the
**core dump** of `tests/expected/*.core.csv`, whose FNV-1a 64 digests are listed in 8.3.

**CSV conventions:** UTF-8, LF line ends, no header except in `*.books_queries.csv`
(`token_id,ts_ns`). A field is quoted only if it holds `,`, `"`, CR or LF, with `"` doubled.

**The full dump (`events.csv`).** Rows in the reference order (5.1), segment by segment:

    E,seg,ev,et,row,out,cont,ts_recv_us,ts_ex_ms,aux,unsynced,extra
    L,seg,ev,lv,token_id,side,price_micro,size_micro,mirror,best_bid,best_ask,hash

| column | meaning |
|---|---|
| `ev` | index in the decoded segment |
| `lv` | index of the level in its event |
| `row` | market row, `nmkt` for type 4 |
| `ts_recv_us` | `floor(ts x unit_ns / 1000)` |
| `ts_ex_ms` | `floor(ts x unit_ns / 1e6) - lat`; checkpoints `0,0` |
| `aux` | fee bps for a trade, else 0 |
| `unsynced` | 1 for a checkpoint listed in `unsynced` |
| `extra` | the tx hash in lower-case hex (type 2), the payload (types 3, 4), or the book hash hex (type 0, with hashes); else empty |
| `side` | `BUY` / `SELL` |
| `price_micro`, `size_micro` | per 3.13 |
| `best_bid`, `best_ask` | the delivered exception strings, or empty |
| `hash` | the entry's hash hex (type 1, with hashes), or empty |

**The core dump.** The same order, without side streams:

    E,seg,idx,et,row,outcome,cont,ts_recv_us,ts_ex_ms,fee_rate_bps
    L,level,outcome,side,price_micro,size_micro,mirror

`side` is 0 or 1 here, and `mirror` 0 or 1.

**`books.csv`.** One row per level: `token_id,ts_ns,side,price_micro,size_micro`, with the BUY rows
then the SELL rows, each in ascending price. A token with no block at or before `ts` gives the one
row `token_id,ts_ns,NONE,,`. A token whose block exists but whose book is empty at `ts` gives no
rows. `vec_renorm_shift0` has both cases.

4. The `.evidx` of `basic_v1.probe` MUST name that file (size, sha256), and its per-block lists
   MUST equal the positions given by the full decode (Appendix A).

### 8.3 Core-dump digests

FNV-1a 64 of the core dump text of each vector:

| vector | events | levels | FNV-1a 64 |
|---|---:|---:|---|
| `basic_v1.probe`, `basic_v2.probe` | 384 | 1,927 | `ace341825691e222` |
| `units_hour_a.probe` | 61 | 237 | `d85b70621dd973d8` |
| `units_hour_b.probe` | 61 | 285 | `d39ccc3ba34af669` |
| `vec_dts_bucket.probe` | 812 | 3,806 | `7ce2bed76353c514` |
| `vec_flags0.probe` | 812 | 3,789 | `d33d4a230dffd4fb` |
| `vec_shift0.probe` | 812 | 3,876 | `6bb5bd17417fe6b6` |
| `vec_renorm_shift0.probe` | 9,004 | 34,157 | `7edc63928a7c4cbd` |
| `vec_shift5.probe` | 812 | 3,955 | `99ed96042c8f5c0c` |
| `vec_size_cache.probe` | 812 | 3,413 | `387bd740338b7e8b` |

On the production hour the digest is `a3211f02123324bf` (63,621,962 events, 163,913,630 levels);
three readers agree on it.

### 8.4 Running the tests

    cargo test --workspace          # this repository's Rust reader and CLI
    probe dump FILE                 # the core dump of any file, with its digest
    probe dump --full FILE          # the full dump

---

## 9. Next version (informative)

Probe separates **the container**, what a file is, from **the codecs**, how each stream inside it
is compressed. The container is the long-term contract: hour files, segments, per-market blocks,
the block and time indexes, the markets table, both clocks and the checkpoints keep their meaning.
The next version (v3, in design) makes the codec of each stream explicit:

- every stream carries a registered codec id and its parameters; a better coder is a new codec id,
  not a new format;
- an id once registered means the same bytes forever, and every codec ships conformance vectors
  before it is registered;
- profiles pick codecs by purpose: **FAST** (static multi-symbol tANS / rANS, interleaved and
  SIMD), **BALANCED** (adaptive multi-symbol coding with the full context models) and **ARCHIVE**
  (the strongest models), all lossless transcodes of one another and of v1 / v2.

Nothing in this section changes v1 or v2.

---

## 10. Glossary

| term | meaning |
|---|---|
| **block** | one market row's events in one segment, or the segment's type-4 events, range-coded with fresh state; decodes alone |
| **block id** | a market row `0..nmkt-1`, or `nmkt` for the type-4 block |
| **bidx** | the segment's block index: row and length per block, plus six counts in v2 |
| **checkpoint** | a type-5 event: a token's full book at its block's start |
| **ckpt bitmap** | per segment, one bit per market row: the row has a block, opening with checkpoints |
| **cont** | the bit marking a book event that is not the first event object of its websocket frame |
| **core dump** | the conformance text of events and levels without side streams (8.2) |
| **dts** | the receive-clock delta from the previous event of the block (from `ts0` at the start) |
| **evmin** | the format's working name, kept in the version strings: evmin/4 = Probe v1, evmin/5 = Probe v2 |
| **exception (best)** | a `"b"` row: a delivered best_bid / best_ask that differs from the rebuilt book |
| **flags** | header bits selecting coding variants (3.11) |
| **gamma** | adaptive Elias-gamma code of a non-negative integer (3.10.4) |
| **held book** | the book the writer holds for a token, which every type-0 diff is taken against |
| **ki / kind** | which model set a size or book field uses: price_change, book or checkpoint |
| **lat** | `floor(receive ms) - venue ms` |
| **level change** | a distinct change of the book: price_change entries other than mirrors, plus snapshot level diffs |
| **market row** | one entry of the market table; a market gets a new row when its units become finer |
| **mirror** | a price_change entry equal to the reflection of the previous explicit entry (other outcome, flipped side, `U - p`, same size), stored as one bit |
| **nmkt** | market rows in use for a segment; also the type-4 block's id |
| **order stream** | the block id of every real event in arrival order |
| **range coder** | the LZMA-style binary arithmetic coder of 3.10 |
| **real event** | any event other than a checkpoint |
| **reference order** | a segment's decoded order: every block's checkpoints by ascending row, then the real events in arrival order |
| **segment** | 300 s of a file, starting at a multiple of 300 s |
| **shift** | the probability adaptation rate (3.10.1) |
| **side stream** | `tx`, `payload.zst` (`"p"`, `"b"`) and `hash`: per-event data kept outside the blocks |
| **token** | one outcome of a market, the unit the venue's book is kept for |
| **U** | price units in 1.0 for a row |
| **unsynced** | a checkpoint synthesized from a held book with no venue snapshot yet |
| **unit_ns** | the receive clock's unit, 1000 ns |
| **zigzag (zz)** | the map of signed to unsigned integers: 0, -1, 1, -2, ... become 0, 1, 2, 3, ... |

---

## Appendix A. The `.evidx` sidecar for v1 files (informative)

An indexer decodes a v1 file once and writes a sidecar holding the same per-block side-stream
positions that v2 has in its `bidx`. With it, a block of a v1 file decodes alone exactly like v2.
Nothing in the v1 file is rewritten.

    "EVX1" | u32 header_len | header JSON | body

Header:

    {"magic":"EVX1","version":"evidx/1","evmin_version":"evmin/4","evmin_bytes":N,
     "evmin_sha256":"<hex of the whole file>","segments":[[offset,length],...]}

`segments` has one entry per segment of the Probe file, with offsets into the sidecar's body. A
reader MUST refuse a sidecar whose `evmin_bytes` or `evmin_sha256` does not match the file.

A segment record lists, per block in `bidx` order:

    uv(n_trade)  n_trade  x uv(d)          positions in tx (32-byte items)
    uv(n_pay)    n_pay    x uv(d)          positions in payload.zst "p"
    uv(n_hashev) n_hashev x uv(d)          first 20-byte hash item of each hashed event
                                           (book: 1 item; price_change: one per entry, contiguous);
                                           n_hashev = 0 when the file has no hash part
    uv(n_bex)    n_bex    x (uv(d) uv(j))  rows of "b", and j = that row's event index in the block
                                           (checkpoints counted)

`d = position - previous position - 1`, with the previous position -1 at the start of each list.
Every list is in the block's event order, so the block's k-th trade is at the k-th position. The
lists hold positions rather than offsets because in v1 a block's items are not contiguous: other
markets' items interleave. A sidecar costs about 1-2 B per trade, payload, hashed event or
exception; the vector `basic_v1.probe.evidx` is 715 B.
