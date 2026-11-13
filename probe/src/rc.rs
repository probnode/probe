//! The binary adaptive range decoder (specification section 3.10): a 32-bit range, 11-bit
//! probabilities of a 0 bit, adaptation shift `shift`, every context starting at p = 0.5. A
//! stream is stored without the encoder's first output byte (always 0), so the code register
//! starts with the next 4 bytes. Bytes past the end of a stream read as 0.
//!
//! With shift 0 (the "two-rate mix", 3.10.1) a context packs two 15-bit counters: `a` in the low
//! half and `b` in the high half. `PROB_INIT` marks a fresh context in both modes.

pub(crate) const PROB_BITS: u32 = 11;
pub(crate) const PROB_INIT: u32 = 1 << (PROB_BITS - 1);
const TOP: u32 = 1 << 24;

/// How many bytes past the end of its stream a decoder may read before the stream is corrupt.
const OVERRUN_SLACK: usize = 4;

pub(crate) struct Rc<'a> {
    buf: &'a [u8],
    pos: usize,
    rng: u32,
    code: u32,
    shift: u32,
}

impl<'a> Rc<'a> {
    pub(crate) fn new(buf: &'a [u8], shift: u32) -> Self {
        let mut d = Rc {
            buf,
            pos: 0,
            rng: 0xFFFF_FFFF,
            code: 0,
            shift,
        };
        for _ in 0..4 {
            d.code = (d.code << 8) | d.next() as u32;
        }
        d
    }

    #[inline(always)]
    fn next(&mut self) -> u8 {
        let b = if self.pos < self.buf.len() {
            self.buf[self.pos]
        } else {
            0
        };
        self.pos = self.pos.saturating_add(1);
        b
    }

    /// The decoder needed more bytes than the stream holds, beyond the zero padding a 5-byte
    /// flush can leave unread.
    #[inline]
    pub(crate) fn overrun(&self) -> bool {
        self.pos > self.buf.len() + OVERRUN_SLACK
    }

    #[inline(always)]
    pub(crate) fn bit(&mut self, p: &mut u32) -> u32 {
        if self.shift == 0 {
            return self.bit_mix(p);
        }
        let bound = (self.rng >> PROB_BITS) * *p;
        let b = if self.code < bound {
            self.rng = bound;
            *p += ((1 << PROB_BITS) - *p) >> self.shift;
            0
        } else {
            self.code -= bound;
            self.rng -= bound;
            *p -= *p >> self.shift;
            1
        };
        // A loop, as the encoder renormalises: with a probability near 0 or 1 one bit can leave
        // the range short of the top more than once (3.10.2).
        while self.rng < TOP {
            self.rng <<= 8;
            self.code = (self.code << 8) | self.next() as u32;
        }
        b
    }

    #[inline(never)]
    fn bit_mix(&mut self, p: &mut u32) -> u32 {
        let (mut a, mut b) = (16384u32, 16384u32);
        if *p != PROB_INIT {
            a = *p & 0xFFFF;
            b = *p >> 16;
        }
        let pr = ((a + b) >> 5).clamp(1, 2047);
        let bound = (self.rng >> PROB_BITS) * pr;
        let bitv = if self.code < bound {
            self.rng = bound;
            a += (32768 - a) >> 4;
            b += (32768 - b) >> 7;
            0
        } else {
            self.code -= bound;
            self.rng -= bound;
            a -= a >> 4;
            b -= b >> 7;
            1
        };
        *p = a | (b << 16);
        while self.rng < TOP {
            self.rng <<= 8;
            self.code = (self.code << 8) | self.next() as u32;
        }
        bitv
    }
}

/// The unary prefix bound: no value below 2^63 needs more.
pub(crate) const MAX_GAMMA_N: usize = 63;

/// The adaptive Elias-gamma code (3.10.4): x = v + 1, n = bitlen(x) - 1, n one-bits and a
/// zero-bit (context = position), then the n low bits of x, most significant first (context =
/// (n, position)). The low-bit contexts of each n live in an arena, allocated on first use.
#[derive(Clone)]
pub(crate) struct Gamma {
    unary: [u32; MAX_GAMMA_N + 1],
    low: [u32; MAX_GAMMA_N + 1], // offset + 1 into the arena; 0 = not used yet
    pub(crate) used: bool,
}

impl Gamma {
    pub(crate) fn new() -> Self {
        Gamma {
            unary: [PROB_INIT; MAX_GAMMA_N + 1],
            low: [0; MAX_GAMMA_N + 1],
            used: false,
        }
    }
}

#[inline]
pub(crate) fn gamma(rc: &mut Rc, g: &mut Gamma, arena: &mut Vec<u32>) -> u64 {
    if !g.used {
        g.used = true;
        g.unary = [PROB_INIT; MAX_GAMMA_N + 1];
        g.low = [0; MAX_GAMMA_N + 1];
    }
    let mut n = 0usize;
    while n < MAX_GAMMA_N && rc.bit(&mut g.unary[n]) == 1 {
        n += 1;
    }
    if n == 0 {
        return 0;
    }
    let mut off = g.low[n];
    if off == 0 {
        off = arena.len() as u32 + 1;
        arena.resize(arena.len() + n, PROB_INIT);
        g.low[n] = off;
    }
    let s = (off - 1) as usize;
    let m = &mut arena[s..s + n];
    let mut x: u64 = 1;
    for p in m.iter_mut() {
        x = (x << 1) | rc.bit(p) as u64;
    }
    x - 1
}

/// The inverse of zigzag: 0, 1, 2, 3, ... become 0, -1, 1, -2, ...
#[inline(always)]
pub(crate) fn unzigzag(u: u64) -> i64 {
    ((u >> 1) as i64) ^ -((u & 1) as i64)
}
