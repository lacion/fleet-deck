//! A test-only builder of an MP4 file as `wf-recorder` writes it: by default a
//! valid 2-second 1920x1080 30 fps `avc1` file with 60 samples of 1,000 bytes
//! in one `mdat`. Each setter breaks or changes one rule.

/// The size of one sample, in bytes.
pub(crate) const SAMPLE: usize = 1000;
/// The delta of one sample, in media time units.
const DELTA: u32 = 1000;
/// The samples of one chunk.
const PER_CHUNK: u32 = 10;

/// A box: the size, the type and the payload.
pub(crate) fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(kind);
    v.extend_from_slice(payload);
    v
}

#[derive(Clone)]
pub(crate) struct Mp4Fixture {
    major: [u8; 4],
    compat: Vec<[u8; 4]>,
    tracks: u32,
    handler: [u8; 4],
    url_flags: u32,
    entry: [u8; 4],
    short_entry: bool,
    moof: bool,
    mvex: bool,
    styp_first: bool,
    offset_past: bool,
    zero_delta: bool,
    zero_size_box: bool,
    co64: bool,
    width: u16,
    height: u16,
    samples: u32,
    timescale: u32,
    moov_first: bool,
    open_mdat: bool,
    wide_mdat: bool,
    free_boxes: u32,
    size_table: bool,
    zero_sample: bool,
    junk: bool,
    v1: bool,
    omit: Option<[u8; 4]>,
}

impl Default for Mp4Fixture {
    fn default() -> Self {
        Self {
            major: *b"isom",
            compat: vec![*b"isom", *b"mp42"],
            tracks: 1,
            handler: *b"vide",
            url_flags: 1,
            entry: *b"avc1",
            short_entry: false,
            moof: false,
            mvex: false,
            styp_first: false,
            offset_past: false,
            zero_delta: false,
            zero_size_box: false,
            co64: false,
            width: 1920,
            height: 1080,
            samples: 60,
            timescale: 30_000,
            moov_first: false,
            open_mdat: false,
            wide_mdat: false,
            free_boxes: 0,
            size_table: false,
            zero_sample: false,
            junk: false,
            v1: false,
            omit: None,
        }
    }
}

impl Mp4Fixture {
    pub(crate) fn brands(mut self, major: [u8; 4], compat: &[[u8; 4]]) -> Self {
        self.major = major;
        self.compat = compat.to_vec();
        self
    }

    pub(crate) fn extra_track(mut self) -> Self {
        self.tracks = 2;
        self
    }

    pub(crate) fn audio_handler(mut self) -> Self {
        self.handler = *b"soun";
        self
    }

    pub(crate) fn url_flags(mut self, flags: u32) -> Self {
        self.url_flags = flags;
        self
    }

    pub(crate) fn avc3(mut self) -> Self {
        self.entry = *b"avc3";
        self
    }

    pub(crate) fn short_avc1(mut self) -> Self {
        self.short_entry = true;
        self
    }

    pub(crate) fn moof(mut self) -> Self {
        self.moof = true;
        self
    }

    pub(crate) fn mvex(mut self) -> Self {
        self.mvex = true;
        self
    }

    pub(crate) fn styp_first(mut self) -> Self {
        self.styp_first = true;
        self
    }

    pub(crate) fn offset_past_mdat(mut self) -> Self {
        self.offset_past = true;
        self
    }

    pub(crate) fn zero_delta(mut self) -> Self {
        self.zero_delta = true;
        self
    }

    /// A `free` box with size 0 before `mdat`.
    pub(crate) fn zero_size_box(mut self) -> Self {
        self.zero_size_box = true;
        self
    }

    pub(crate) fn co64(mut self) -> Self {
        self.co64 = true;
        self
    }

    pub(crate) fn size(mut self, width: u16, height: u16) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    pub(crate) fn samples(mut self, samples: u32) -> Self {
        self.samples = samples;
        self
    }

    /// `2 × fps` samples in 2 seconds.
    pub(crate) fn fps(mut self, fps: u32) -> Self {
        self.samples = 2 * fps;
        self.timescale = fps * DELTA;
        self
    }

    pub(crate) fn moov_first(mut self) -> Self {
        self.moov_first = true;
        self
    }

    /// The `mdat` has size 0: it extends to the end of the file.
    pub(crate) fn open_mdat(mut self) -> Self {
        self.open_mdat = true;
        self
    }

    /// The `mdat` has a 64-bit size.
    pub(crate) fn wide_mdat(mut self) -> Self {
        self.wide_mdat = true;
        self
    }

    pub(crate) fn free_boxes(mut self, n: u32) -> Self {
        self.free_boxes = n;
        self
    }

    pub(crate) fn size_table(mut self) -> Self {
        self.size_table = true;
        self
    }

    /// A size table where the 4th sample has size 0.
    pub(crate) fn zero_sample(mut self) -> Self {
        self.zero_sample = true;
        self
    }

    /// An unknown box at the end of every container.
    pub(crate) fn junk(mut self) -> Self {
        self.junk = true;
        self
    }

    /// Version 1 of `mvhd` and `mdhd`.
    pub(crate) fn v1(mut self) -> Self {
        self.v1 = true;
        self
    }

    /// Leave out every box of this type.
    pub(crate) fn omit(mut self, kind: &[u8; 4]) -> Self {
        self.omit = Some(*kind);
        self
    }

    pub(crate) fn build(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.styp_first {
            out.extend(self.b(b"styp", &[]));
        }
        out.extend(self.ftyp());
        for _ in 0..self.free_boxes {
            out.extend(bx(b"free", &[]));
        }
        if self.zero_size_box {
            out.extend([0, 0, 0, 0, b'f', b'r', b'e', b'e']);
        }
        let header = if self.wide_mdat { 16 } else { 8 };
        let after_header = out.len() as u64 + header;
        let moov_len = self.moov(0).len() as u64;
        let base = if self.moov_first {
            after_header + moov_len
        } else {
            after_header
        };
        let (moov, mdat) = (self.moov(base), self.mdat());
        if self.moov_first {
            out.extend(moov);
            out.extend(mdat);
        } else {
            out.extend(mdat);
            out.extend(moov);
        }
        if self.moof {
            out.extend(bx(b"moof", &[]));
        }
        out
    }

    fn b(&self, kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        if self.omit == Some(*kind) {
            return Vec::new();
        }
        bx(kind, payload)
    }

    fn cont(&self, kind: &[u8; 4], mut kids: Vec<Vec<u8>>) -> Vec<u8> {
        if self.omit == Some(*kind) {
            return Vec::new();
        }
        if self.junk {
            kids.push(bx(b"zzzz", &[1, 2, 3]));
        }
        bx(kind, &kids.concat())
    }

    fn mdat_len(&self) -> usize {
        self.samples as usize * SAMPLE
    }

    fn ftyp(&self) -> Vec<u8> {
        let mut p = self.major.to_vec();
        p.extend([0u8; 4]);
        p.extend(self.compat.concat());
        self.b(b"ftyp", &p)
    }

    fn mdat(&self) -> Vec<u8> {
        if self.omit == Some(*b"mdat") {
            return Vec::new();
        }
        let body = vec![0u8; self.mdat_len()];
        let mut v = Vec::new();
        if self.open_mdat {
            v.extend([0, 0, 0, 0]);
            v.extend(b"mdat");
        } else if self.wide_mdat {
            v.extend(1u32.to_be_bytes());
            v.extend(b"mdat");
            v.extend((body.len() as u64 + 16).to_be_bytes());
        } else {
            return bx(b"mdat", &body);
        }
        v.extend(body);
        v
    }

    fn moov(&self, base: u64) -> Vec<u8> {
        let mut kids = vec![self.mvhd()];
        for _ in 0..self.tracks {
            kids.push(self.trak(base));
        }
        if self.mvex {
            kids.push(self.b(b"mvex", &[]));
        }
        self.cont(b"moov", kids)
    }

    /// The `mvhd` and `mdhd` payload after the version byte: the times, the
    /// timescale, the duration and `rest` bytes.
    fn header_box(&self, kind: &[u8; 4], rest: usize) -> Vec<u8> {
        let duration = self.samples * DELTA;
        let mut p = Vec::new();
        if self.v1 {
            p.extend([1, 0, 0, 0]);
            p.extend([0u8; 16]);
            p.extend(self.timescale.to_be_bytes());
            p.extend(u64::from(duration).to_be_bytes());
        } else {
            p.extend([0u8; 12]);
            p.extend(self.timescale.to_be_bytes());
            p.extend(duration.to_be_bytes());
        }
        p.extend(vec![0u8; rest]);
        self.b(kind, &p)
    }

    fn mvhd(&self) -> Vec<u8> {
        self.header_box(b"mvhd", 80)
    }

    fn trak(&self, base: u64) -> Vec<u8> {
        let tkhd = self.b(b"tkhd", &[0u8; 84]);
        let mut hdlr = vec![0u8; 8];
        hdlr.extend(self.handler);
        hdlr.extend([0u8; 12]);
        hdlr.extend(b"VideoHandler\0");
        let vmhd = self.b(b"vmhd", &[0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
        let mut dref = vec![0, 0, 0, 0, 0, 0, 0, 1];
        dref.extend(bx(b"url ", &self.url_flags.to_be_bytes()));
        let dinf = self.cont(b"dinf", vec![self.b(b"dref", &dref)]);
        let stbl = self.cont(
            b"stbl",
            vec![
                self.stsd(),
                self.stts(),
                self.stsc(),
                self.stsz(),
                self.offsets(base),
            ],
        );
        let minf = self.cont(b"minf", vec![vmhd, dinf, stbl]);
        let mdia = self.cont(
            b"mdia",
            vec![self.header_box(b"mdhd", 4), self.b(b"hdlr", &hdlr), minf],
        );
        self.cont(b"trak", vec![tkhd, mdia])
    }

    fn stsd(&self) -> Vec<u8> {
        let mut e = vec![0u8; 6];
        e.extend(1u16.to_be_bytes());
        e.extend([0u8; 16]);
        e.extend(self.width.to_be_bytes());
        e.extend(self.height.to_be_bytes());
        e.extend([0u8; 50]);
        if self.short_entry {
            e.truncate(40);
        } else {
            e.extend(self.b(b"avcC", &[1, 0x64, 0, 0x1f, 0xff, 0xe1, 0, 0]));
        }
        let mut p = vec![0, 0, 0, 0, 0, 0, 0, 1];
        p.extend(bx(&self.entry, &e));
        self.b(b"stsd", &p)
    }

    fn stts(&self) -> Vec<u8> {
        let delta = if self.zero_delta { 0 } else { DELTA };
        let mut p = vec![0, 0, 0, 0, 0, 0, 0, 1];
        p.extend(self.samples.to_be_bytes());
        p.extend(u32::to_be_bytes(delta));
        self.b(b"stts", &p)
    }

    fn chunks(&self) -> u32 {
        self.samples.div_ceil(PER_CHUNK)
    }

    fn stsc(&self) -> Vec<u8> {
        let (full, rest) = (self.samples / PER_CHUNK, self.samples % PER_CHUNK);
        let mut entries = Vec::new();
        if full > 0 {
            entries.push((1u32, PER_CHUNK));
        }
        if rest > 0 {
            entries.push((full + 1, rest));
        }
        let mut p = vec![0u8; 4];
        p.extend((entries.len() as u32).to_be_bytes());
        for (first, per) in entries {
            p.extend(first.to_be_bytes());
            p.extend(per.to_be_bytes());
            p.extend(1u32.to_be_bytes());
        }
        self.b(b"stsc", &p)
    }

    fn stsz(&self) -> Vec<u8> {
        let table = self.size_table || self.zero_sample;
        let mut p = vec![0u8; 4];
        let fixed: u32 = if table { 0 } else { SAMPLE as u32 };
        p.extend(fixed.to_be_bytes());
        p.extend(self.samples.to_be_bytes());
        if table {
            for i in 0..self.samples {
                let size = if self.zero_sample && i == 3 {
                    0
                } else {
                    SAMPLE as u32
                };
                p.extend(size.to_be_bytes());
            }
        }
        self.b(b"stsz", &p)
    }

    fn offsets(&self, base: u64) -> Vec<u8> {
        let mut p = vec![0u8; 4];
        p.extend(self.chunks().to_be_bytes());
        for i in 0..u64::from(self.chunks()) {
            let mut off = base + i * u64::from(PER_CHUNK) * SAMPLE as u64;
            if self.offset_past && i == 0 {
                off = base + self.mdat_len() as u64 + 10;
            }
            if self.co64 {
                p.extend(off.to_be_bytes());
            } else {
                p.extend((off as u32).to_be_bytes());
            }
        }
        self.b(if self.co64 { b"co64" } else { b"stco" }, &p)
    }
}
