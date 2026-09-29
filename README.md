# compress

A lightweight Rust CLI for PDFs, JPEGs, and PNGs. No Rust dependencies.
Balanced compression is the default; originals are never overwritten.

```sh
compress report.pdf photo.jpg screenshot.png
compress --quality high report.pdf
compress --quality balanced report.pdf
compress --quality small report.pdf
compress --lossless report.pdf
```

| Mode | PDF color/grayscale image target | JPEG quality |
| --- | --- | --- |
| high | 300 DPI | 90 |
| balanced (default) | 150 DPI | 80 |
| small | 96 DPI | 60 |
| lossless | No downsampling/re-encoding | Lossless optimization |

The three presets allow quality loss. PDF downsampling occurs only when images
exceed Ghostscript's threshold; small images are not enlarged. Output sizes
are content-dependent and not guaranteed to follow a strict ordering.
PDF JPEG quantization factors are 0.4, 0.76, and 1.3 respectively; these are
Ghostscript settings, distinct from standalone JPEG quality numbers.
PNG/APNG always use lossless optimization, regardless of quality setting.
`--lossless` and `--quality` cannot be combined. `--quality=small` also works.

Results are saved beside the input as `report.compressed.pdf`, only when smaller.
Existing outputs cause an error; move or rename an old result before rerunning.
No output is written when the result would be larger. Files run sequentially.
Routing uses contents, not extensions. Unsupported formats, directories and
symlinks are rejected. Use quotes for spaces and `--` for dash-prefixed filenames.

```sh
compress "my report.pdf"
compress -- -report.pdf
compress *.png
compress --help
compress --version
```

## Installation

With Rust 1.66+ and Python 3 available for packaging, run from this directory:

```sh
./scripts/install-homebrew.sh
```

This builds a native binary, generates a checksum-pinned local Homebrew formula,
and installs or reinstalls `local/compress/compress-cli`. Backends are declared as
dependencies. Neither Python nor Rust is required at runtime. Run in a normal
terminal; Homebrew's nested sandbox may fail inside a restricted agent session.

To build and use directly:

```sh
brew install qpdf ghostscript jpegoptim oxipng
cargo build --release --locked --offline
./target/release/compress --quality balanced document.pdf
```

macOS has an unrelated `/usr/bin/compress`. Put Homebrew's bin first on PATH or
use `$(brew --prefix)/bin/compress`. Check `compress --version`: these flags
require version 0.2.0 or later. Building locally does not update an installed copy;
rerun the installer to update it.

The generated formula uses a local native archive in `dist/`. Keep it for
reinstallation. A public tap is not published. For distribution, host archives
for each OS/architecture and update the formula URLs/checksums accordingly.
Linux binaries inherit the build system's libc requirements.

## Backends and safety

- PDF: qpdf validates input and output. Lossless mode uses qpdf stream
  optimization. Quality presets use Ghostscript pdfwrite to downsample images
  and recompress. Ghostscript rewrites documents and may alter or discard
  interactive features, annotations or metadata. Use lossless mode for documents
  whose structure must be preserved. Any PDF rewrite can invalidate signatures;
  retain signed originals. Password-required PDFs fail without prompting.
- JPEG: jpegoptim uses the selected quality cap, or coefficient optimization in
  lossless mode. Markers and trailing data are retained. Output is checked with
  a no-action pass.
- PNG/APNG: oxipng level 2, retaining metadata and using its internal output
  validation. Transparency and animation are retained.
- All backends operate on private copies. A final signature/size check rejects
  obvious bad results. Backend checks are not proof of visual equivalence.
- A synced result is published using an atomic no-clobber hard link, protecting
  existing files even if another process creates the destination while working.
  Filesystems without hard-link support fail safely. Ordinary permission bits
  are retained; timestamps, ACLs, and extended attributes are not copied.
- SIGINT/SIGTERM kill the active backend and clean up temporary files. SIGKILL,
  a crash, or power loss may leave `.compress-*` directories. Remove them only
  when no compressor is running. Already completed batch outputs remain.
- Do not modify an input during compression: copying is not a filesystem
  snapshot. Output directory entries are not fsynced against power-loss rollback.

Exit codes: 0 = success/no savings; 1 = file/backend error; 2 = usage error;
130 = interrupted. Failed files do not prevent subsequent files being processed.
Missing backends include a Homebrew install command. Supported OS: macOS/Linux.

## Tests

```sh
cargo test --locked --offline
cargo clippy --offline --all-targets -- -D warnings
cargo build --release --locked --offline
COMPRESS_REAL=1 python3 -m unittest discover -s tests -v
```

Tests cover original preservation, collision races, dangling symlinks, malformed
output, missing tools, interruptions, batch failures, filenames, preset arguments,
and JPEG quality routing. Real tests use generated PDFs/PNGs and macOS sips JPEGs.
