//! `cortex-pkg` — portable archive CLI (P0 #2).
//!
//! ```text
//! cortex-pkg export <out.cortex-pkg.tar.gz> [--force-orphans] [--data-dir <dir>]
//! cortex-pkg verify <archive.cortex-pkg.tar.gz>
//! cortex-pkg import <archive.cortex-pkg.tar.gz> <target-dir> [--force]
//! ```
//!
//! Data dir resolution matches the desktop app: `--data-dir` → `CORTEX_DATA_DIR`
//! → platform app-data dir for `app.study.cortex`. Exit codes: 0 ok, 1 failure,
//! 2 bad usage.

use std::process::ExitCode;

fn data_dir_from(args: &[String]) -> Option<std::path::PathBuf> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--data-dir" && i + 1 < args.len() {
            return Some(std::path::PathBuf::from(&args[i + 1]));
        }
        i += 1;
    }
    None
}

fn usage() -> ExitCode {
    eprintln!(
        "cortex-pkg — portable Cortex library archive (.cortex-pkg.tar.gz)\n\
         \n\
         Usage:\n\
           cortex-pkg export <out.tar.gz> [--force-orphans] [--data-dir <dir>]\n\
           cortex-pkg verify <archive.tar.gz>\n\
           cortex-pkg import <archive.tar.gz> <target-dir> [--force]\n\
         \n\
         Env: CORTEX_DATA_DIR (default: platform app-data dir).\n\
         The archive is manifest.json + cortex.db + sources/* with per-file\n\
         SHA-256; verify hashes everything, import refuses a tampered archive."
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" {
        return usage();
    }
    let mut progress = |stage: &str, pct: u8| {
        eprintln!("[{pct:>3}%] {stage}");
    };
    match args[0].as_str() {
        "export" => {
            let Some(out) = args.get(1) else { return usage() };
            let force = args.iter().any(|a| a == "--force-orphans");
            let data_dir = data_dir_from(&args).unwrap_or_else(|| {
                cortex_lib::mcp_server::default_db_path()
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_default()
            });
            match cortex_lib::pkg::export(
                &data_dir,
                std::path::Path::new(out),
                force,
                &mut progress,
            ) {
                Ok(r) => {
                    println!(
                        "exported {} files ({:.1} MB) from {} sources → {}\nsha256 {}",
                        r.files,
                        r.bytes as f64 / 1_048_576.0,
                        r.counts.sources,
                        r.out,
                        r.sha256
                    );
                    if !r.orphans.is_empty() {
                        println!("skipped orphans ({}): {}", r.orphans.len(), r.orphans.join(", "));
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("cortex-pkg export: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        "verify" => {
            let Some(pkg) = args.get(1) else { return usage() };
            match cortex_lib::pkg::verify(std::path::Path::new(pkg)) {
                Ok(r) if r.ok => {
                    println!(
                        "OK — {} files verified, {} sources, exported {} (v{})",
                        r.files,
                        r.counts.map(|c| c.sources).unwrap_or(0),
                        r.created_at.unwrap_or(0),
                        r.app_version.unwrap_or_default()
                    );
                    ExitCode::SUCCESS
                }
                Ok(r) => {
                    eprintln!("FAILED — {} problem(s):", r.problems.len());
                    for p in &r.problems {
                        eprintln!("  - {p}");
                    }
                    ExitCode::FAILURE
                }
                Err(e) => {
                    eprintln!("cortex-pkg verify: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        "import" => {
            let (Some(pkg), Some(target)) = (args.get(1), args.get(2)) else { return usage() };
            let force = args.iter().any(|a| a == "--force");
            match cortex_lib::pkg::import(
                std::path::Path::new(pkg),
                std::path::Path::new(target),
                force,
            ) {
                Ok(r) => {
                    println!(
                        "imported {} files into {} ({} sources, {} notes, {} papers; repointed {})",
                        r.files,
                        target,
                        r.counts.sources,
                        r.counts.notes,
                        r.counts.papers,
                        r.repointed
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("cortex-pkg import: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        other => {
            eprintln!("unknown subcommand: {other}");
            usage()
        }
    }
}
