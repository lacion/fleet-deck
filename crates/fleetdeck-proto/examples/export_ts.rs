//! Writes the TypeScript bindings: `cargo run -p fleetdeck-proto --example export_ts -- <dir>`.
//! `make ts-types` runs it for `web/src/generated`.

use std::path::PathBuf;

fn main() -> std::io::Result<()> {
    let Some(dir) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: export_ts <out-dir>");
        std::process::exit(2);
    };
    if dir.is_dir() {
        // Remove only the files that this tool generates, so that stale
        // bindings of renamed types disappear.
        for file in fleetdeck_proto::ts::binding_files(&dir)? {
            std::fs::remove_file(dir.join(file))?;
        }
    }
    std::fs::create_dir_all(&dir)?;
    fleetdeck_proto::ts::export_all(&dir)?;
    println!("wrote TypeScript bindings to {}", dir.display());
    Ok(())
}
