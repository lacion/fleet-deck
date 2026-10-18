//! The sample tables of the video track (`stbl`): the sample entry, the
//! timing, the sizes, and the chunk mapping. A table is read in a stream with a
//! small buffer, only up to the entry count that the limits allow.

use std::io::{Read, Seek};

use super::{BoxHdr, Mp4Error, Rd, Res, be16, be32, be64, set, version};

/// The size of the read buffer of a table; a multiple of 4, 8 and 12.
const BUF: usize = 3840;
/// The size of the visual sample entry header.
const VISUAL_ENTRY: u64 = 78;

/// Where a table starts and how many entries it has.
#[derive(Clone, Copy)]
struct Loc {
    pos: u64,
    count: u64,
}

/// How the sample sizes are stored.
#[derive(Clone, Copy)]
enum Sizes {
    Fixed(u32),
    Table(Loc),
}

/// What the walk of `stbl` finds; the extents check uses the locations.
pub(super) struct Stbl {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) samples: u64,
    pub(super) media_duration: u64,
    pub(super) last_delta: u64,
    stsc: Loc,
    sizes: Sizes,
    offsets: Loc,
    co64: bool,
}

/// A stream over the fixed-size records of one table.
struct Table {
    pos: u64,
    left: u64,
    rec: usize,
    buf: [u8; BUF],
    have: usize,
    at: usize,
}

impl Table {
    fn new(loc: Loc, rec: usize) -> Self {
        Self {
            pos: loc.pos,
            left: loc.count,
            rec,
            buf: [0; BUF],
            have: 0,
            at: 0,
        }
    }

    fn next<R: Read + Seek>(&mut self, rd: &mut Rd<'_, R>) -> Res<Option<&[u8]>> {
        if self.at >= self.have {
            if self.left == 0 {
                return Ok(None);
            }
            let n = self.left.min((BUF / self.rec) as u64);
            let bytes = (n as usize) * self.rec;
            rd.read_at(self.pos, &mut self.buf[..bytes])?;
            self.pos = self
                .pos
                .checked_add(bytes as u64)
                .ok_or(Mp4Error::Structure)?;
            self.left -= n;
            self.have = bytes;
            self.at = 0;
        }
        let from = self.at;
        self.at += self.rec;
        self.buf
            .get(from..from + self.rec)
            .map(Some)
            .ok_or(Mp4Error::Structure)
    }

    /// The next record as `N` big-endian 32-bit words.
    fn words<R: Read + Seek, const N: usize>(
        &mut self,
        rd: &mut Rd<'_, R>,
    ) -> Res<Option<[u32; N]>> {
        let Some(s) = self.next(rd)? else {
            return Ok(None);
        };
        let mut out = [0u32; N];
        for (i, o) in out.iter_mut().enumerate() {
            *o = be32(s, i * 4)?;
        }
        Ok(Some(out))
    }

    fn long<R: Read + Seek>(&mut self, rd: &mut Rd<'_, R>) -> Res<Option<u64>> {
        let Some(s) = self.next(rd)? else {
            return Ok(None);
        };
        be64(s, 0).map(Some)
    }
}

/// The location of a table box: the full-box header, the entry count, then
/// `count` records of `rec` bytes.
fn table_box<R: Read + Seek>(rd: &mut Rd<'_, R>, h: &BoxHdr, rec: u64) -> Res<Loc> {
    if h.len() < 8 {
        return Err(Mp4Error::Structure);
    }
    let mut hd = [0u8; 8];
    rd.read_at(h.payload, &mut hd)?;
    version(hd[0])?;
    let count = u64::from(be32(&hd, 4)?);
    if count > rd.lim.max_samples {
        return Err(Mp4Error::Limits);
    }
    if count.checked_mul(rec).ok_or(Mp4Error::Structure)? > h.len() - 8 {
        return Err(Mp4Error::Truncated);
    }
    Ok(Loc {
        pos: h.payload + 8,
        count,
    })
}

/// The boxes of `stbl` that the validator uses.
#[derive(Default)]
struct Found {
    stsd: Option<BoxHdr>,
    stts: Option<BoxHdr>,
    stsc: Option<BoxHdr>,
    stsz: Option<BoxHdr>,
    stco: Option<BoxHdr>,
    co64: Option<BoxHdr>,
}

pub(super) fn parse_stbl<R: Read + Seek>(rd: &mut Rd<'_, R>, h: &BoxHdr) -> Res<Stbl> {
    let mut f = Found::default();
    rd.each(h.payload, h.end, 6, |_, c| {
        let slot = match &c.kind {
            b"stsd" => &mut f.stsd,
            b"stts" => &mut f.stts,
            b"stsc" => &mut f.stsc,
            b"stsz" => &mut f.stsz,
            b"stco" => &mut f.stco,
            b"co64" => &mut f.co64,
            _ => return Ok(()),
        };
        set(slot, *c)
    })?;
    let need = |b: Option<BoxHdr>| b.ok_or(Mp4Error::Structure);
    let (width, height) = stsd(rd, &need(f.stsd)?)?;
    let dimensions_ok = width % 2 == 0
        && height % 2 == 0
        && (2..=8192).contains(&width)
        && (2..=8192).contains(&height)
        && u64::from(width) * u64::from(height) <= 16_777_216;
    if !dimensions_ok {
        return Err(Mp4Error::Dimensions);
    }
    let (samples, media_duration, last_delta) = stts(rd, &need(f.stts)?)?;
    let sizes = stsz(rd, &need(f.stsz)?, samples)?;
    let stsc = table_box(rd, &need(f.stsc)?, 12)?;
    let (offsets, co64) = match (f.stco, f.co64) {
        (Some(c), None) => (table_box(rd, &c, 4)?, false),
        (None, Some(c)) => (table_box(rd, &c, 8)?, true),
        _ => return Err(Mp4Error::Structure),
    };
    Ok(Stbl {
        width,
        height,
        samples,
        media_duration,
        last_delta,
        stsc,
        sizes,
        offsets,
        co64,
    })
}

/// Exactly one `avc1` entry with an `avcC` box; returns its width and height.
fn stsd<R: Read + Seek>(rd: &mut Rd<'_, R>, h: &BoxHdr) -> Res<(u32, u32)> {
    if h.len() < 8 {
        return Err(Mp4Error::Structure);
    }
    let mut hd = [0u8; 8];
    rd.read_at(h.payload, &mut hd)?;
    version(hd[0])?;
    if be32(&hd, 4)? != 1 {
        return Err(Mp4Error::Codec);
    }
    let e = rd.header(h.payload + 8, h.end, 7, false)?;
    if &e.kind != b"avc1" || e.len() < VISUAL_ENTRY {
        return Err(Mp4Error::Codec);
    }
    let mut v = [0u8; 28];
    rd.read_at(e.payload, &mut v)?;
    let (width, height) = (u32::from(be16(&v, 24)?), u32::from(be16(&v, 26)?));
    let mut avcc = false;
    rd.each(e.payload + VISUAL_ENTRY, e.end, 8, |_, c| {
        avcc |= &c.kind == b"avcC";
        Ok(())
    })?;
    if !avcc {
        return Err(Mp4Error::Codec);
    }
    Ok((width, height))
}

/// The sample count, the media duration and the last delta from `stts`.
fn stts<R: Read + Seek>(rd: &mut Rd<'_, R>, h: &BoxHdr) -> Res<(u64, u64, u64)> {
    let mut t = Table::new(table_box(rd, h, 8)?, 8);
    let (mut total, mut duration, mut last) = (0u64, 0u64, 0u64);
    while let Some([count, delta]) = t.words::<_, 2>(rd)? {
        if count == 0 {
            return Err(Mp4Error::Structure);
        }
        if delta == 0 {
            return Err(Mp4Error::Timing);
        }
        total = total
            .checked_add(u64::from(count))
            .ok_or(Mp4Error::Structure)?;
        if total > rd.lim.max_samples {
            return Err(Mp4Error::Limits);
        }
        duration = u64::from(count)
            .checked_mul(u64::from(delta))
            .and_then(|v| v.checked_add(duration))
            .ok_or(Mp4Error::Structure)?;
        last = u64::from(delta);
    }
    if total == 0 {
        return Err(Mp4Error::Timing);
    }
    Ok((total, duration, last))
}

/// A fixed size or a table of `total` sizes.
fn stsz<R: Read + Seek>(rd: &mut Rd<'_, R>, h: &BoxHdr, total: u64) -> Res<Sizes> {
    if h.len() < 12 {
        return Err(Mp4Error::Structure);
    }
    let mut hd = [0u8; 12];
    rd.read_at(h.payload, &mut hd)?;
    version(hd[0])?;
    let (fixed, count) = (be32(&hd, 4)?, u64::from(be32(&hd, 8)?));
    if count != total {
        return Err(Mp4Error::Structure);
    }
    if fixed != 0 {
        return Ok(Sizes::Fixed(fixed));
    }
    if count.checked_mul(4).ok_or(Mp4Error::Structure)? > h.len() - 12 {
        return Err(Mp4Error::Truncated);
    }
    Ok(Sizes::Table(Loc {
        pos: h.payload + 12,
        count,
    }))
}

/// Place every sample with the chunk mapping and check that its extent lies in
/// one `mdat` payload. Sample data is never read.
pub(super) fn check_extents<R: Read + Seek>(
    rd: &mut Rd<'_, R>,
    s: &Stbl,
    mdats: &[(u64, u64)],
) -> Res<()> {
    // The first-chunk entries start at 1 and strictly increase.
    let mut t = Table::new(s.stsc, 12);
    let mut prev = 0u32;
    while let Some([first, per_chunk, _]) = t.words::<_, 3>(rd)? {
        if (prev == 0 && first != 1) || first <= prev || per_chunk == 0 {
            return Err(Mp4Error::Structure);
        }
        prev = first;
    }
    if prev == 0 {
        return Err(Mp4Error::Structure);
    }
    let mut runs = Table::new(s.stsc, 12);
    let mut upcoming = runs.words::<_, 3>(rd)?;
    let mut per_chunk = 0u32;
    let mut offsets = Table::new(s.offsets, if s.co64 { 8 } else { 4 });
    let mut sizes = match s.sizes {
        Sizes::Fixed(_) => None,
        Sizes::Table(loc) => Some(Table::new(loc, 4)),
    };
    let (mut left, mut chunk) = (s.samples, 0u64);
    while left > 0 {
        let off = if s.co64 {
            offsets.long(rd)?
        } else {
            offsets.words::<_, 1>(rd)?.map(|[v]| u64::from(v))
        };
        // Too few chunks to place every sample.
        let off = off.ok_or(Mp4Error::Structure)?;
        chunk += 1;
        while let Some([first, spc, _]) = upcoming {
            if u64::from(first) > chunk {
                break;
            }
            per_chunk = spc;
            upcoming = runs.words::<_, 3>(rd)?;
        }
        let at = mdats.partition_point(|&(start, _)| start <= off);
        let (_, mdat_end) = *at
            .checked_sub(1)
            .and_then(|i| mdats.get(i))
            .ok_or(Mp4Error::Extents)?;
        if off >= mdat_end {
            return Err(Mp4Error::Extents);
        }
        let mut pos = off;
        for _ in 0..per_chunk {
            if left == 0 {
                break;
            }
            let size = match (&mut sizes, s.sizes) {
                (Some(t), _) => t
                    .words::<_, 1>(rd)?
                    .map(|[v]| v)
                    .ok_or(Mp4Error::Structure)?,
                (None, Sizes::Fixed(v)) => v,
                (None, Sizes::Table(_)) => return Err(Mp4Error::Structure),
            };
            if size == 0 {
                return Err(Mp4Error::Extents);
            }
            pos = pos
                .checked_add(u64::from(size))
                .ok_or(Mp4Error::Structure)?;
            if pos > mdat_end {
                return Err(Mp4Error::Extents);
            }
            left -= 1;
        }
    }
    Ok(())
}
