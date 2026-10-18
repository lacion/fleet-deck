//! The structural validator for the MP4 files that `wf-recorder` writes
//! (design `omarchy-p5b` §9.3 steps 2-7). It is pure and bounded: it walks the
//! boxes with seeks and small buffers, never loads `mdat` or the whole file,
//! uses checked arithmetic, and needs no `ffprobe` or decoder. It does not
//! claim to decode H.264.

use std::io::{ErrorKind, Read, Seek, SeekFrom};

#[path = "mp4_tables.rs"]
mod tables;
#[cfg(test)]
#[path = "mp4_tests.rs"]
mod tests;

/// The compatible brands that the recorder profile accepts.
const BRANDS: [&[u8; 4]; 8] = [
    b"isom", b"iso2", b"iso4", b"iso5", b"iso6", b"mp41", b"mp42", b"avc1",
];
/// The most compatible brands in `ftyp`.
const MAX_BRANDS: u64 = 64;
/// The most bytes read of an `hdlr` payload.
const HDLR_CAP: u64 = 256;
/// The most bytes read of any other fixed-size full box.
const FULL_CAP: u64 = 128;

/// The limits of one validation.
#[derive(Debug, Clone)]
pub struct Mp4Limits {
    pub max_bytes: u64,
    pub max_depth: u32,
    pub max_boxes: u32,
    pub max_samples: u64,
    pub max_fps: u32,
}

impl Mp4Limits {
    /// The limits for a recording of at most `seconds` seconds (§9.3).
    pub fn for_seconds(seconds: u32) -> Self {
        Self {
            max_bytes: 268_435_456,
            max_depth: 16,
            max_boxes: 100_000,
            max_samples: u64::from(seconds) * 30,
            max_fps: 30,
        }
    }
}

/// What a valid file declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mp4Info {
    pub width: u32,
    pub height: u32,
    pub duration_ms: u64,
    pub samples: u64,
}

/// Why a file is not valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mp4Error {
    Truncated,
    Structure,
    Fragmented,
    Tracks,
    Codec,
    DataRef,
    Extents,
    Dimensions,
    Timing,
    Limits,
    Io,
}

type Res<T> = Result<T, Mp4Error>;

/// Validate the MP4 file behind `r`. `size` is the file size from `fstat`;
/// the validator never reads past it.
pub fn validate<R: Read + Seek>(r: &mut R, size: u64, l: &Mp4Limits) -> Result<Mp4Info, Mp4Error> {
    if size > l.max_bytes {
        return Err(Mp4Error::Limits);
    }
    if size < 8 {
        return Err(Mp4Error::Truncated);
    }
    Rd {
        r,
        size,
        lim: l,
        boxes: 0,
    }
    .file()
}

/// A box header: the type, where the payload starts and where the box ends.
#[derive(Clone, Copy)]
struct BoxHdr {
    kind: [u8; 4],
    payload: u64,
    end: u64,
}

impl BoxHdr {
    fn len(&self) -> u64 {
        self.end - self.payload
    }
}

struct Rd<'a, R> {
    r: &'a mut R,
    size: u64,
    lim: &'a Mp4Limits,
    boxes: u32,
}

/// One track after the movie walk: what it declares, and where its tables are.
struct Track {
    info: Mp4Info,
    stbl: tables::Stbl,
}

struct Mdhd {
    timescale: u32,
    duration: u64,
}

fn arr<const N: usize>(b: &[u8], o: usize) -> Res<[u8; N]> {
    let end = o.checked_add(N).ok_or(Mp4Error::Structure)?;
    b.get(o..end)
        .and_then(|s| <[u8; N]>::try_from(s).ok())
        .ok_or(Mp4Error::Structure)
}

fn be16(b: &[u8], o: usize) -> Res<u16> {
    arr::<2>(b, o).map(u16::from_be_bytes)
}

fn be32(b: &[u8], o: usize) -> Res<u32> {
    arr::<4>(b, o).map(u32::from_be_bytes)
}

fn be64(b: &[u8], o: usize) -> Res<u64> {
    arr::<8>(b, o).map(u64::from_be_bytes)
}

/// Fill a slot that a box may fill only once.
fn set<T>(slot: &mut Option<T>, v: T) -> Res<()> {
    if slot.is_some() {
        return Err(Mp4Error::Structure);
    }
    *slot = Some(v);
    Ok(())
}

/// A full box has the version 0 or 1.
fn version(v: u8) -> Res<()> {
    if v > 1 {
        return Err(Mp4Error::Structure);
    }
    Ok(())
}

impl<R: Read + Seek> Rd<'_, R> {
    /// Read exactly `buf.len()` bytes at `off`, inside the declared size.
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> Res<()> {
        let end = off
            .checked_add(buf.len() as u64)
            .ok_or(Mp4Error::Structure)?;
        if end > self.size {
            return Err(Mp4Error::Truncated);
        }
        self.r
            .seek(SeekFrom::Start(off))
            .map_err(|_| Mp4Error::Io)?;
        self.r.read_exact(buf).map_err(|e| {
            if e.kind() == ErrorKind::UnexpectedEof {
                Mp4Error::Truncated
            } else {
                Mp4Error::Io
            }
        })
    }

    /// The header of the box at `off` inside a parent that ends at `pend`.
    fn header(&mut self, off: u64, pend: u64, depth: u32, top: bool) -> Res<BoxHdr> {
        if depth > self.lim.max_depth {
            return Err(Mp4Error::Limits);
        }
        self.boxes = self.boxes.saturating_add(1);
        if self.boxes > self.lim.max_boxes {
            return Err(Mp4Error::Limits);
        }
        let avail = pend.saturating_sub(off);
        if avail < 8 {
            return Err(Mp4Error::Truncated);
        }
        let mut h = [0u8; 8];
        self.read_at(off, &mut h)?;
        let kind = [h[4], h[5], h[6], h[7]];
        let (hlen, size) = match u32::from_be_bytes([h[0], h[1], h[2], h[3]]) {
            0 => {
                // The box extends to the end of the file: only the last
                // top-level `mdat` may do that.
                if !(top && &kind == b"mdat") {
                    return Err(Mp4Error::Structure);
                }
                (8, avail)
            }
            1 => {
                if avail < 16 {
                    return Err(Mp4Error::Truncated);
                }
                let mut b = [0u8; 8];
                self.read_at(off + 8, &mut b)?;
                (16, u64::from_be_bytes(b))
            }
            n => (8, u64::from(n)),
        };
        if size < hlen {
            return Err(Mp4Error::Truncated);
        }
        let end = off.checked_add(size).ok_or(Mp4Error::Structure)?;
        if end > pend {
            return Err(Mp4Error::Truncated);
        }
        Ok(BoxHdr {
            kind,
            payload: off + hlen,
            end,
        })
    }

    /// Call `f` for each child box in `start..end`; the children are at `depth`.
    fn each<F>(&mut self, start: u64, end: u64, depth: u32, mut f: F) -> Res<()>
    where
        F: FnMut(&mut Self, &BoxHdr) -> Res<()>,
    {
        let mut off = start;
        while off < end {
            let h = self.header(off, end, depth, false)?;
            f(self, &h)?;
            off = h.end;
        }
        Ok(())
    }

    /// The payload of a small box, up to `cap` bytes.
    fn payload(&mut self, h: &BoxHdr, cap: u64) -> Res<Vec<u8>> {
        if h.len() > cap {
            return Err(Mp4Error::Limits);
        }
        let n = usize::try_from(h.len()).map_err(|_| Mp4Error::Limits)?;
        let mut v = vec![0u8; n];
        self.read_at(h.payload, &mut v)?;
        Ok(v)
    }

    /// The top-level walk (§9.3 steps 3-7).
    fn file(&mut self) -> Res<Mp4Info> {
        let (mut off, mut first) = (0u64, true);
        let mut track: Option<Track> = None;
        let mut moov_seen = false;
        let mut mdats: Vec<(u64, u64)> = Vec::new();
        while off < self.size {
            let h = self.header(off, self.size, 1, true)?;
            if matches!(&h.kind, b"moof" | b"mfra" | b"sidx" | b"styp") {
                return Err(Mp4Error::Fragmented);
            }
            if first && &h.kind != b"ftyp" {
                return Err(Mp4Error::Structure);
            }
            match &h.kind {
                b"ftyp" if first => self.ftyp(&h)?,
                b"moov" => {
                    if moov_seen {
                        return Err(Mp4Error::Structure);
                    }
                    moov_seen = true;
                    track = Some(self.moov(&h)?);
                }
                b"mdat" => mdats.push((h.payload, h.end)),
                b"free" | b"skip" | b"uuid" | b"udta" | b"meta" | b"wide" => {}
                _ => return Err(Mp4Error::Structure),
            }
            first = false;
            off = h.end;
        }
        let track = track.ok_or(Mp4Error::Structure)?;
        if mdats.is_empty() {
            return Err(Mp4Error::Structure);
        }
        tables::check_extents(self, &track.stbl, &mdats)?;
        Ok(track.info)
    }

    fn ftyp(&mut self, h: &BoxHdr) -> Res<()> {
        let p = self.payload(h, 8 + 4 * MAX_BRANDS)?;
        if p.len() < 8 || (p.len() - 8) % 4 != 0 {
            return Err(Mp4Error::Structure);
        }
        let major = arr::<4>(&p, 0)?;
        let compatible = p[8..]
            .chunks_exact(4)
            .any(|c| BRANDS.iter().any(|b| *b == c));
        if BRANDS.iter().any(|b| **b == major) || compatible {
            Ok(())
        } else {
            Err(Mp4Error::Structure)
        }
    }

    fn moov(&mut self, h: &BoxHdr) -> Res<Track> {
        let mut mvhd: Option<()> = None;
        let mut track: Option<Track> = None;
        let mut traks = 0u32;
        self.each(h.payload, h.end, 2, |rd, c| {
            match &c.kind {
                b"mvhd" => {
                    rd.mvhd(c)?;
                    set(&mut mvhd, ())?;
                }
                b"mvex" => return Err(Mp4Error::Fragmented),
                b"trak" => {
                    traks += 1;
                    if traks > 1 {
                        return Err(Mp4Error::Tracks);
                    }
                    track = Some(rd.trak(c)?);
                }
                _ => {}
            }
            Ok(())
        })?;
        mvhd.ok_or(Mp4Error::Structure)?;
        track.ok_or(Mp4Error::Tracks)
    }

    fn mvhd(&mut self, h: &BoxHdr) -> Res<()> {
        let p = self.payload(h, FULL_CAP)?;
        let first = *p.first().ok_or(Mp4Error::Structure)?;
        version(first)?;
        let at = if first == 0 { 12 } else { 20 };
        if be32(&p, at)? == 0 {
            return Err(Mp4Error::Timing);
        }
        Ok(())
    }

    fn trak(&mut self, h: &BoxHdr) -> Res<Track> {
        let mut tkhd: Option<()> = None;
        let mut track: Option<Track> = None;
        self.each(h.payload, h.end, 3, |rd, c| {
            match &c.kind {
                b"tkhd" => set(&mut tkhd, ())?,
                b"mdia" => set(&mut track, rd.mdia(c)?)?,
                _ => {}
            }
            Ok(())
        })?;
        tkhd.ok_or(Mp4Error::Structure)?;
        track.ok_or(Mp4Error::Structure)
    }

    fn mdia(&mut self, h: &BoxHdr) -> Res<Track> {
        let mut mdhd: Option<Mdhd> = None;
        let mut hdlr: Option<()> = None;
        let mut stbl: Option<tables::Stbl> = None;
        self.each(h.payload, h.end, 4, |rd, c| {
            match &c.kind {
                b"mdhd" => set(&mut mdhd, rd.mdhd(c)?)?,
                b"hdlr" => {
                    rd.hdlr(c)?;
                    set(&mut hdlr, ())?;
                }
                b"minf" => set(&mut stbl, rd.minf(c)?)?,
                _ => {}
            }
            Ok(())
        })?;
        let mdhd = mdhd.ok_or(Mp4Error::Structure)?;
        hdlr.ok_or(Mp4Error::Structure)?;
        let stbl = stbl.ok_or(Mp4Error::Structure)?;
        // The header duration and the table sum agree to one sample delta.
        if stbl.media_duration.abs_diff(mdhd.duration) > stbl.last_delta {
            return Err(Mp4Error::Timing);
        }
        let ms = stbl
            .media_duration
            .checked_mul(1000)
            .ok_or(Mp4Error::Structure)?
            / u64::from(mdhd.timescale);
        if ms == 0 {
            return Err(Mp4Error::Timing);
        }
        // The frame-rate ceiling, with one frame of tolerance.
        let lhs = stbl.samples.checked_mul(1000).ok_or(Mp4Error::Structure)?;
        let rhs = u64::from(self.lim.max_fps)
            .checked_mul(ms)
            .and_then(|v| v.checked_add(1000))
            .ok_or(Mp4Error::Structure)?;
        if lhs > rhs {
            return Err(Mp4Error::Timing);
        }
        let info = Mp4Info {
            width: stbl.width,
            height: stbl.height,
            duration_ms: ms,
            samples: stbl.samples,
        };
        Ok(Track { info, stbl })
    }

    fn mdhd(&mut self, h: &BoxHdr) -> Res<Mdhd> {
        let p = self.payload(h, FULL_CAP)?;
        let first = *p.first().ok_or(Mp4Error::Structure)?;
        version(first)?;
        let (timescale, duration) = if first == 0 {
            (be32(&p, 12)?, u64::from(be32(&p, 16)?))
        } else {
            (be32(&p, 20)?, be64(&p, 24)?)
        };
        if timescale == 0 || duration == 0 {
            return Err(Mp4Error::Timing);
        }
        Ok(Mdhd {
            timescale,
            duration,
        })
    }

    fn hdlr(&mut self, h: &BoxHdr) -> Res<()> {
        let p = self.payload(h, HDLR_CAP)?;
        if &arr::<4>(&p, 8)? != b"vide" {
            return Err(Mp4Error::Tracks);
        }
        Ok(())
    }

    fn minf(&mut self, h: &BoxHdr) -> Res<tables::Stbl> {
        let mut vmhd: Option<()> = None;
        let mut dinf: Option<()> = None;
        let mut stbl: Option<tables::Stbl> = None;
        self.each(h.payload, h.end, 5, |rd, c| {
            match &c.kind {
                b"vmhd" => set(&mut vmhd, ())?,
                b"dinf" => {
                    rd.dinf(c)?;
                    set(&mut dinf, ())?;
                }
                b"stbl" => set(&mut stbl, tables::parse_stbl(rd, c)?)?,
                _ => {}
            }
            Ok(())
        })?;
        vmhd.ok_or(Mp4Error::Tracks)?;
        dinf.ok_or(Mp4Error::Structure)?;
        stbl.ok_or(Mp4Error::Structure)
    }

    fn dinf(&mut self, h: &BoxHdr) -> Res<()> {
        let mut dref: Option<()> = None;
        self.each(h.payload, h.end, 6, |rd, c| {
            if &c.kind == b"dref" {
                rd.dref(c)?;
                set(&mut dref, ())?;
            }
            Ok(())
        })?;
        dref.ok_or(Mp4Error::Structure)
    }

    /// Exactly one entry: a `url ` whose flag bit 0 says "self-contained".
    fn dref(&mut self, h: &BoxHdr) -> Res<()> {
        if h.len() < 8 {
            return Err(Mp4Error::Structure);
        }
        let mut hd = [0u8; 8];
        self.read_at(h.payload, &mut hd)?;
        version(hd[0])?;
        if be32(&hd, 4)? != 1 {
            return Err(Mp4Error::DataRef);
        }
        let e = self.header(h.payload + 8, h.end, 7, false)?;
        if &e.kind != b"url " {
            return Err(Mp4Error::DataRef);
        }
        if e.len() < 4 {
            return Err(Mp4Error::Structure);
        }
        let mut flags = [0u8; 4];
        self.read_at(e.payload, &mut flags)?;
        if flags[3] & 1 == 0 {
            return Err(Mp4Error::DataRef);
        }
        Ok(())
    }
}
