//! A minimal MP4 builder for the fake `wf-recorder` (a copy of the default
//! path of the test-only `mp4_fixture` of the station crate, which another
//! crate cannot use): one `avc1` video track in one `mdat`, `samples` samples
//! of 1,000 bytes at 30 fps. The `mdat` bytes are a function of their
//! position, so a wrong offset in a read shows in the bytes.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

/// The size of one sample of an ordinary clip, in bytes.
const SAMPLE: u32 = 1000;
/// The delta of one sample, in media time units.
const DELTA: u32 = 1000;
/// The media timescale: 30 samples are one second.
const TIMESCALE: u32 = 30_000;
/// The samples of one chunk.
const PER_CHUNK: u32 = 10;

fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = u32::try_from(payload.len() + 8)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    v.extend_from_slice(kind);
    v.extend_from_slice(payload);
    v
}

/// The `mvhd` and `mdhd` box: the times, the timescale, the duration and
/// `rest` zero bytes.
fn header_box(kind: &[u8; 4], duration: u32, rest: usize) -> Vec<u8> {
    let mut p = vec![0u8; 12];
    p.extend(TIMESCALE.to_be_bytes());
    p.extend(duration.to_be_bytes());
    p.extend(vec![0u8; rest]);
    bx(kind, &p)
}

/// A clip of `seconds` seconds at 30 fps for a monitor of `width` x `height`.
pub fn clip(width: u16, height: u16, seconds: u32) -> Vec<u8> {
    big_clip(width, height, seconds, SAMPLE)
}

/// The same clip with samples of `sample` bytes (a large file).
pub fn big_clip(width: u16, height: u16, seconds: u32, sample: u32) -> Vec<u8> {
    Mp4 {
        width,
        height,
        samples: seconds * 30,
        sample,
    }
    .build()
}

struct Mp4 {
    width: u16,
    height: u16,
    samples: u32,
    sample: u32,
}

impl Mp4 {
    fn mdat_len(&self) -> usize {
        self.samples as usize * self.sample as usize
    }

    fn build(&self) -> Vec<u8> {
        let mut ftyp = b"isom".to_vec();
        ftyp.extend([0u8; 4]);
        ftyp.extend(b"isom");
        ftyp.extend(b"mp42");
        let mut out = bx(b"ftyp", &ftyp);
        // The chunk offsets point into the `mdat` that follows `ftyp`.
        let base = out.len() as u64 + 8;
        let body: Vec<u8> = (0..self.mdat_len()).map(|i| (i % 251) as u8).collect();
        out.extend(bx(b"mdat", &body));
        out.extend(self.moov(base));
        out
    }

    fn moov(&self, base: u64) -> Vec<u8> {
        let mvhd = header_box(b"mvhd", self.samples * DELTA, 80);
        bx(b"moov", &[mvhd, self.trak(base)].concat())
    }

    fn trak(&self, base: u64) -> Vec<u8> {
        let tkhd = bx(b"tkhd", &[0u8; 84]);
        let mut hdlr = vec![0u8; 8];
        hdlr.extend(b"vide");
        hdlr.extend([0u8; 12]);
        hdlr.extend(b"VideoHandler\0");
        let vmhd = bx(b"vmhd", &[0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
        let mut dref = vec![0, 0, 0, 0, 0, 0, 0, 1];
        dref.extend(bx(b"url ", &1u32.to_be_bytes()));
        let dinf = bx(b"dinf", &bx(b"dref", &dref));
        let stbl = bx(
            b"stbl",
            &[
                self.stsd(),
                self.stts(),
                self.stsc(),
                self.stsz(),
                self.stco(base),
            ]
            .concat(),
        );
        let minf = bx(b"minf", &[vmhd, dinf, stbl].concat());
        let mdia = bx(
            b"mdia",
            &[
                header_box(b"mdhd", self.samples * DELTA, 4),
                bx(b"hdlr", &hdlr),
                minf,
            ]
            .concat(),
        );
        bx(b"trak", &[tkhd, mdia].concat())
    }

    fn stsd(&self) -> Vec<u8> {
        let mut e = vec![0u8; 6];
        e.extend(1u16.to_be_bytes());
        e.extend([0u8; 16]);
        e.extend(self.width.to_be_bytes());
        e.extend(self.height.to_be_bytes());
        e.extend([0u8; 50]);
        e.extend(bx(b"avcC", &[1, 0x64, 0, 0x1f, 0xff, 0xe1, 0, 0]));
        let mut p = vec![0, 0, 0, 0, 0, 0, 0, 1];
        p.extend(bx(b"avc1", &e));
        bx(b"stsd", &p)
    }

    fn stts(&self) -> Vec<u8> {
        let mut p = vec![0, 0, 0, 0, 0, 0, 0, 1];
        p.extend(self.samples.to_be_bytes());
        p.extend(DELTA.to_be_bytes());
        bx(b"stts", &p)
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
        p.extend(u32::try_from(entries.len()).unwrap().to_be_bytes());
        for (first, per) in entries {
            p.extend(first.to_be_bytes());
            p.extend(per.to_be_bytes());
            p.extend(1u32.to_be_bytes());
        }
        bx(b"stsc", &p)
    }

    fn stsz(&self) -> Vec<u8> {
        let mut p = vec![0u8; 4];
        p.extend(self.sample.to_be_bytes());
        p.extend(self.samples.to_be_bytes());
        bx(b"stsz", &p)
    }

    fn stco(&self, base: u64) -> Vec<u8> {
        let mut p = vec![0u8; 4];
        p.extend(self.chunks().to_be_bytes());
        for i in 0..u64::from(self.chunks()) {
            let off = base + i * u64::from(PER_CHUNK) * u64::from(self.sample);
            p.extend(u32::try_from(off).unwrap().to_be_bytes());
        }
        bx(b"stco", &p)
    }
}
