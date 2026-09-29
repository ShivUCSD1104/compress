use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::{
    env,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    thread,
    time::Duration,
};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
extern "C" fn interrupted(_: i32) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}
extern "C" {
    fn signal(sig: i32, handler: extern "C" fn(i32)) -> usize;
}
const HELP: &str = "compress — compress PDFs, JPEGs, and PNGs\n\nUsage: compress [--lossless | --quality high|balanced|small] FILE...\n\nDefault: balanced (lossy PDF/JPEG compression).\n--lossless       Preserve image quality; no downsampling\n--quality LEVEL  high: 300 dpi / JPEG 90; balanced: 150 dpi / JPEG 80; small: 96 dpi / JPEG 60\nPNG/APNG always use lossless optimization.\n--help, -h       Show help\n--version, -V    Show version\n\nWrites FILE.compressed.EXT only when smaller. Never overwrites files.\nUse -- before filenames beginning with a dash.\nBackends: brew install qpdf ghostscript jpegoptim oxipng\n";

#[derive(Clone, Copy, Debug, PartialEq)]
enum Quality {
    High,
    Balanced,
    Small,
}
impl Quality {
    fn parse(value: &OsString) -> Result<Self> {
        match value.to_str() {
            Some("high") => Ok(Self::High),
            Some("balanced") => Ok(Self::Balanced),
            Some("small") => Ok(Self::Small),
            _ => Err("--quality must be high, balanced, or small".into()),
        }
    }
    fn settings(self) -> (u32, u32) {
        match self {
            Self::High => (300, 90),
            Self::Balanced => (150, 80),
            Self::Small => (96, 60),
        }
    }
}
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Pdf,
    Jpeg,
    Png,
}
impl Kind {
    fn tool(self) -> &'static str {
        match self {
            Self::Pdf => "qpdf",
            Self::Jpeg => "jpegoptim",
            Self::Png => "oxipng",
        }
    }
}
fn identify(path: &Path) -> Result<Kind> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = [0u8; 8];
    let n = f.read(&mut bytes).map_err(|e| e.to_string())?;
    let b = &bytes[..n];
    if b.starts_with(b"%PDF-") {
        Ok(Kind::Pdf)
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        Ok(Kind::Jpeg)
    } else if b == b"\x89PNG\r\n\x1a\n" {
        Ok(Kind::Png)
    } else {
        Err("unsupported format (supported: PDF, JPEG, PNG; detected by contents)".into())
    }
}
fn destination(path: &Path) -> PathBuf {
    let mut name = path.file_stem().unwrap_or_default().to_os_string();
    name.push(".compressed");
    if let Some(ext) = path.extension() {
        name.push(".");
        name.push(ext);
    }
    path.with_file_name(name)
}
struct Scratch(PathBuf);
impl Scratch {
    fn new(parent: &Path) -> Result<Self> {
        for _ in 0..100 {
            let path = parent.join(format!(
                ".compress-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("cannot create temporary directory: {e}")),
            }
        }
        Err("cannot allocate temporary directory".into())
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn run(tool: &str, args: &[OsString], scratch: &Scratch) -> Result<()> {
    if INTERRUPTED.load(Ordering::SeqCst) {
        return Err("interrupted".into());
    }
    let log_path = scratch.0.join("backend.log");
    let log = File::create(&log_path).map_err(|e| e.to_string())?;
    let mut child = Command::new(tool)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                format!(
                    "missing {tool}; install it with: brew install {}",
                    if tool == "gs" { "ghostscript" } else { tool }
                )
            } else {
                format!("cannot start {tool}: {e}")
            }
        })?;
    loop {
        if INTERRUPTED.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("interrupted".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                let mut detail = String::new();
                if let Ok(f) = File::open(&log_path) {
                    let _ = f.take(4096).read_to_string(&mut detail);
                }
                return Err(format!("{tool} failed ({status}): {}", detail.trim()));
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.to_string());
            }
        }
    }
}
fn args(parts: &[&str], path: &Path) -> Vec<OsString> {
    let mut a: Vec<_> = parts.iter().map(OsString::from).collect();
    a.push(path.as_os_str().into());
    a
}
fn compress(input: &Path, quality: Option<Quality>) -> Result<String> {
    // Do not resolve the final component: refuse symlinks instead of following them.
    let path = env::current_dir().map_err(|e| e.to_string())?.join(input);
    let meta = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    if !meta.file_type().is_file() {
        return Err("input must be a regular file, not a symlink or directory".into());
    }
    let kind = identify(&path)?;
    let dest = destination(&path);
    match fs::symlink_metadata(&dest) {
        Ok(_) => return Err(format!("output already exists: {}", dest.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.to_string()),
    }
    let scratch = Scratch::new(path.parent().ok_or("missing parent directory")?)?;
    let source = scratch.0.join("input");
    fs::copy(&path, &source).map_err(|e| format!("cannot snapshot input: {e}"))?;
    // Backends can mutate only a private, writable copy.
    fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    let original_size = fs::metadata(&source).map_err(|e| e.to_string())?.len();
    let output = scratch.0.join("output");
    match kind {
        Kind::Pdf => {
            run("qpdf", &args(&["--check"], &source), &scratch)?;
            if let Some(level) = quality {
                let (dpi, _) = level.settings();
                let mut a: Vec<OsString> = [
                    "-q",
                    "-dSAFER",
                    "-dBATCH",
                    "-dNOPAUSE",
                    "-sDEVICE=pdfwrite",
                    "-dAutoRotatePages=/None",
                    "-dDetectDuplicateImages=true",
                    "-dDownsampleColorImages=true",
                    "-dDownsampleGrayImages=true",
                    "-dColorImageDownsampleType=/Bicubic",
                    "-dGrayImageDownsampleType=/Bicubic",
                    "-dPassThroughJPEGImages=false",
                    "-dPassThroughJPXImages=false",
                ]
                .iter()
                .map(OsString::from)
                .collect();
                a.push(format!("-dColorImageResolution={dpi}").into());
                a.push(format!("-dGrayImageResolution={dpi}").into());

                let mut output_arg = OsString::from("-sOutputFile=");
                output_arg.push(&output);
                a.push(output_arg);
                let (factor, samples) = match level {
                    Quality::High => ("0.4", "1 1 1 1"),
                    Quality::Balanced => ("0.76", "2 1 1 2"),
                    Quality::Small => ("1.3", "2 1 1 2"),
                };
                let dict = format!(
                    "<< /QFactor {factor} /Blend 1 /HSamples [{samples}] /VSamples [{samples}] >>"
                );
                a.push("-c".into());
                a.push(format!("<< /ColorImageDict {dict} /GrayImageDict {dict} /ColorACSImageDict {dict} /GrayACSImageDict {dict} >> setdistillerparams").into());
                a.push("-f".into());
                a.push(source.as_os_str().into());
                run("gs", &a, &scratch)?;
            } else {
                let mut a = args(
                    &[
                        "--object-streams=generate",
                        "--recompress-flate",
                        "--compression-level=6",
                    ],
                    &source,
                );
                a.push(output.as_os_str().into());
                run("qpdf", &a, &scratch)?;
            }
            run("qpdf", &args(&["--check"], &output), &scratch)?;
        }
        Kind::Jpeg => {
            fs::copy(&source, &output).map_err(|e| e.to_string())?;
            let mut a = args(
                &["--quiet", "--strip-none", "--nofix", "--save-extra"],
                &output,
            );
            a.pop();
            if let Some(level) = quality {
                a.push(format!("--max={}", level.settings().1).into());
            }
            a.push("--".into());
            a.push(output.as_os_str().into());
            run("jpegoptim", &a, &scratch)?;
            run(
                "jpegoptim",
                &args(&["--quiet", "--noaction", "--nofix", "--"], &output),
                &scratch,
            )?;
        }
        Kind::Png => {
            fs::copy(&source, &output).map_err(|e| e.to_string())?;
            // oxipng verifies decoded output internally; never pass --fix or --strip.
            run(
                "oxipng",
                &args(&["--quiet", "-o", "2", "--"], &output),
                &scratch,
            )?;
        }
    }
    if identify(&output)? != kind {
        return Err(format!("{} produced an invalid output format", kind.tool()));
    }
    let size = fs::metadata(&output).map_err(|e| e.to_string())?.len();
    if size == 0 {
        return Err("backend produced an empty file".into());
    }
    if size >= original_size {
        return Ok("unchanged (no smaller result)".into());
    }
    if INTERRUPTED.load(Ordering::SeqCst) {
        return Err("interrupted".into());
    }
    fs::set_permissions(
        &output,
        fs::Permissions::from_mode(meta.permissions().mode() & 0o777),
    )
    .map_err(|e| e.to_string())?;
    OpenOptions::new()
        .read(true)
        .open(&output)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())?;
    // A same-filesystem hard link atomically publishes without replacing any existing path.
    fs::hard_link(&output, &dest).map_err(|e| {
        format!(
            "cannot publish {} (nothing overwritten): {e}",
            dest.display()
        )
    })?;
    Ok(format!(
        "{} → {} (saved {:.1}%, {} bytes)",
        original_size,
        size,
        100.0 * (original_size - size) as f64 / original_size as f64,
        original_size - size
    ))
}
fn main() {
    unsafe {
        signal(2, interrupted);
        signal(15, interrupted);
    }
    let mut paths = Vec::new();
    let mut literal = false;
    let mut quality = None;
    let mut lossless = false;
    let mut argv = env::args_os().skip(1);
    while let Some(arg) = argv.next() {
        if !literal {
            if arg == "--lossless" {
                lossless = true;
                continue;
            }
            if arg == "--quality" || arg.to_string_lossy().starts_with("--quality=") {
                let value = if arg == "--quality" {
                    argv.next()
                } else {
                    arg.to_str()
                        .and_then(|s| s.strip_prefix("--quality="))
                        .map(OsString::from)
                };
                quality = match value
                    .as_ref()
                    .ok_or_else(|| "--quality requires high, balanced, or small".to_string())
                    .and_then(Quality::parse)
                {
                    Ok(q) => Some(q),
                    Err(e) => {
                        eprintln!("compress: {e}");
                        std::process::exit(2);
                    }
                };
                continue;
            }
            if arg == "--" {
                literal = true;
                continue;
            }
            if arg == "--help" || arg == "-h" {
                print!("{HELP}");
                return;
            }
            if arg == "--version" || arg == "-V" {
                println!("compress {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            if arg.to_string_lossy().starts_with('-') {
                eprintln!(
                    "compress: unknown option {}; use --help",
                    arg.to_string_lossy()
                );
                std::process::exit(2);
            }
        }
        paths.push(PathBuf::from(arg));
    }
    if lossless && quality.is_some() {
        eprintln!("compress: --lossless cannot be combined with --quality");
        std::process::exit(2);
    }
    let mode = if lossless {
        None
    } else {
        Some(quality.unwrap_or(Quality::Balanced))
    };
    if paths.is_empty() {
        print!("{HELP}");
        std::process::exit(2);
    }
    let mut failed = false;
    for path in paths {
        match compress(&path, mode) {
            Ok(message) => println!("{}: {message}", path.display()),
            Err(error) => {
                eprintln!("{}: {error}", path.display());
                failed = true;
            }
        }
        if INTERRUPTED.load(Ordering::SeqCst) {
            std::process::exit(130);
        }
    }
    if failed {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_names() {
        assert_eq!(
            destination(Path::new("a/photo.JPG")),
            Path::new("a/photo.compressed.JPG")
        );
        assert_eq!(destination(Path::new("scan")), Path::new("scan.compressed"));
        assert_eq!(
            destination(Path::new("a.b.pdf")),
            Path::new("a.b.compressed.pdf")
        );
    }
    #[test]
    fn scratch_does_not_reuse_existing_directory() {
        let parent = env::temp_dir();
        let a = Scratch::new(&parent).unwrap();
        let b = Scratch::new(&parent).unwrap();
        assert_ne!(a.0, b.0);
        assert_eq!(
            fs::metadata(&a.0).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let p = a.0.clone();
        drop(a);
        assert!(!p.exists());
    }
}
