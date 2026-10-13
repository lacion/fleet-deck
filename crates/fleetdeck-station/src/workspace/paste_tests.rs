use super::*;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

fn png(extra: usize) -> Vec<u8> {
    let mut b = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    b.extend(std::iter::repeat_n(7u8, extra));
    b
}

fn req(id: &str, ext: &str, bytes: &[u8]) -> PasteImage {
    PasteImage {
        upload_id: id.to_owned(),
        ext: ext.to_owned(),
        data_base64: STANDARD.encode(bytes),
    }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn count(dir: &Path) -> usize {
    fs::read_dir(dir).unwrap().count()
}

fn set_age(path: &Path, now: SystemTime, age: Duration) {
    let file = fs::File::options().write(true).open(path).unwrap();
    file.set_modified(now - age).unwrap();
}

fn code(r: Result<PastedImage, PasteError>) -> &'static str {
    r.unwrap_err().code
}

#[test]
fn a_good_png_is_stored_with_private_modes() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("paste");
    let bytes = png(100);
    let got = write(&dir, &req("upload-0001", "png", &bytes), SystemTime::now()).unwrap();
    let path = Path::new(&got.path);
    assert!(path.is_absolute());
    assert_eq!(path.parent().unwrap(), dir);
    assert_eq!(path.extension().unwrap(), "png");
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert_eq!(mode(path), 0o600);
    assert_eq!(mode(&dir), 0o700);
    // No temp file stays.
    assert_eq!(count(&dir), 1);
}

#[test]
fn the_same_id_and_bytes_return_the_same_path() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("paste");
    let r = req("upload-0001", "png", &png(10));
    let a = write(&dir, &r, SystemTime::now()).unwrap();
    let b = write(&dir, &r, SystemTime::now()).unwrap();
    assert_eq!(a, b);
    assert_eq!(count(&dir), 1);
}

#[test]
fn the_same_id_with_other_bytes_is_a_conflict() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("paste");
    let first = write(
        &dir,
        &req("upload-0001", "png", &png(10)),
        SystemTime::now(),
    )
    .unwrap();
    let err = write(
        &dir,
        &req("upload-0001", "png", &png(11)),
        SystemTime::now(),
    )
    .unwrap_err();
    assert_eq!(err.code, "conflict");
    assert_eq!(fs::read(&first.path).unwrap(), png(10));
}

#[test]
fn every_extension_needs_its_magic_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("paste");
    let now = SystemTime::now();
    let webp = [b"RIFF".as_slice(), &[1, 2, 3, 4], b"WEBPVP8 "].concat();
    assert!(
        write(
            &dir,
            &req("upload-jpg1", "jpg", &[0xFF, 0xD8, 0xFF, 0xE0]),
            now
        )
        .is_ok()
    );
    assert!(write(&dir, &req("upload-gif1", "gif", b"GIF89a.."), now).is_ok());
    assert!(write(&dir, &req("upload-gif2", "gif", b"GIF87a.."), now).is_ok());
    assert!(write(&dir, &req("upload-webp", "webp", &webp), now).is_ok());
    // PNG bytes under another extension, and a text file as a PNG.
    let err = write(&dir, &req("upload-bad1", "jpg", &png(10)), now).unwrap_err();
    assert_eq!(err.code, "invalid_params");
    assert_eq!(err.message, "the data is not a jpg image");
    assert_eq!(
        code(write(&dir, &req("upload-bad2", "png", b"hello world"), now)),
        "invalid_params"
    );
    assert_eq!(
        code(write(
            &dir,
            &req("upload-bad3", "webp", b"RIFF1234WAVE"),
            now
        )),
        "invalid_params"
    );
}

#[test]
fn the_size_limits_are_one_byte_to_700_kib() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("paste");
    let now = SystemTime::now();
    let ok = png(MAX_BYTES - 8);
    assert_eq!(ok.len(), MAX_BYTES);
    assert!(write(&dir, &req("upload-edge", "png", &ok), now).is_ok());
    let big = png(MAX_BYTES - 8 + 1);
    assert_eq!(
        code(write(&dir, &req("upload-big1", "png", &big), now)),
        "too_large"
    );
    assert_eq!(
        code(write(&dir, &req("upload-empt", "png", &[]), now)),
        "too_large"
    );
    // A text far above the limit is refused before the decode.
    let huge = PasteImage {
        upload_id: "upload-huge".to_owned(),
        ext: "png".to_owned(),
        data_base64: "A".repeat(2_000_000),
    };
    assert_eq!(code(write(&dir, &huge, now)), "too_large");
}

#[test]
fn bad_parameters_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("paste");
    let now = SystemTime::now();
    for id in [
        "short",
        "has space1",
        "dots.not.ok",
        "../../etc/x1",
        &"a".repeat(65),
    ] {
        assert_eq!(
            code(write(&dir, &req(id, "png", &png(4)), now)),
            "invalid_params",
            "{id}"
        );
    }
    assert!(write(&dir, &req(&"a".repeat(64), "png", &png(4)), now).is_ok());
    assert!(write(&dir, &req("A-b_9xyz", "png", &png(4)), now).is_ok());
    assert_eq!(
        code(write(&dir, &req("upload-0001", "svg", &png(4)), now)),
        "invalid_params"
    );
    let bad = PasteImage {
        upload_id: "upload-0002".to_owned(),
        ext: "png".to_owned(),
        data_base64: "not base64!".to_owned(),
    };
    assert_eq!(code(write(&dir, &bad, now)), "invalid_params");
}

#[test]
fn fifty_files_shrink_to_fifty_after_a_write() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("paste");
    let now = SystemTime::now();
    fs::create_dir(&dir).unwrap();
    for i in 0..50 {
        let path = dir.join(format!("f{i:02}.png"));
        fs::write(&path, png(4)).unwrap();
        set_age(&path, now, Duration::from_secs(3600 + (50 - i) * 60));
    }
    write(&dir, &req("upload-0001", "png", &png(10)), now).unwrap();
    assert_eq!(count(&dir), 50);
    // The oldest is gone: 49 old files stay and the new file is the 50th.
    assert!(!dir.join("f00.png").exists());
    assert!(dir.join("f01.png").exists());
    assert!(dir.join("f49.png").exists());
}

#[test]
fn a_file_older_than_24_hours_is_deleted() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("paste");
    let now = SystemTime::now();
    fs::create_dir(&dir).unwrap();
    let old = dir.join("old.png");
    let fresh = dir.join("fresh.png");
    fs::write(&old, png(4)).unwrap();
    fs::write(&fresh, png(4)).unwrap();
    set_age(&old, now, Duration::from_secs(25 * 3600));
    set_age(&fresh, now, Duration::from_secs(23 * 3600));
    write(&dir, &req("upload-0001", "png", &png(10)), now).unwrap();
    assert!(!old.exists());
    assert!(fresh.exists());
}

#[test]
fn the_byte_quota_deletes_the_oldest_files() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("paste");
    let now = SystemTime::now();
    fs::create_dir(&dir).unwrap();
    // Four files of 7 MiB: 28 MiB, above 24 MiB.
    for i in 0..4u64 {
        let path = dir.join(format!("big{i}.png"));
        fs::write(&path, vec![0u8; 7 * 1024 * 1024]).unwrap();
        set_age(&path, now, Duration::from_secs(3600 * (5 - i)));
    }
    write(&dir, &req("upload-0001", "png", &png(10)), now).unwrap();
    assert!(!dir.join("big0.png").exists());
    assert!(dir.join("big1.png").exists());
    assert!(dir.join("big3.png").exists());
}

#[test]
fn a_link_in_place_of_the_directory_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("elsewhere");
    fs::create_dir(&target).unwrap();
    let dir = tmp.path().join("paste");
    std::os::unix::fs::symlink(&target, &dir).unwrap();
    let err = write(&dir, &req("upload-0001", "png", &png(4)), SystemTime::now()).unwrap_err();
    assert_eq!(err.code, "internal");
    assert_eq!(count(&target), 0);
}
