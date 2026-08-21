# issue-3-image-pipeline — Test Contract

## Functional Behavior

- `resize(bytes, w, h)` decodes JPEG or PNG bytes, center-crops to the requested aspect ratio, and scales to exactly `w × h` pixels.
- Transparent pixels are composited deterministically against white; grayscale and extreme-aspect-ratio inputs are accepted.
- Zero target dimensions and undecodable bytes return `ImagePipelineError`; production image code does not panic.
- `extract_palette(img)` uses deterministic two-cluster color quantization and always returns `[dominant, accent]` as lowercase `#rrggbb` strings.
- The dominant color is ordered by cluster population. The accent is HSL-clamped to lightness `0.35..=0.75` and saturation `>= 0.4`, including monochrome black and white inputs.
- `dither(img, DitherMode::Bayer)` uses ordered Bayer dithering; `DitherMode::Atkinson` uses Atkinson error diffusion.
- Dither output is packed row-major and MSB-first, pads every row to a whole byte, treats `1` as dark foreground ink, and stores the bytes as base64 in `Art.bits`.
- `process(bytes, w, h, mode)` returns the resized image's palette and an `Art` whose dimensions and dither mode match the request.
- Every operation is deterministic for identical inputs and parameters.

## Unit Tests

- `image::tests::resize_decodes_png_and_jpeg_to_exact_dimensions` — both supported formats decode, center-crop, and scale to the requested size.
- `image::tests::resize_rejects_zero_dimensions_and_invalid_bytes` — invalid requests return errors.
- `image::tests::resize_handles_degenerate_valid_inputs` — 1×1, extreme aspect ratio, grayscale, and transparent PNG fixtures succeed without panic.
- `image::tests::palette_clamps_black_and_white_accents` — both monochrome fixtures return exactly two colors and compliant accent HSL values.
- `image::tests::palette_is_deterministic_and_dominant_first` — repeated extraction is byte-identical and the larger color region is first.
- `image::tests::packed_length_matches_padded_rows` — decoded bytes equal `ceil(w/8) * h` for `w ∈ {1,7,8,9,400}` and `h ∈ {1,3,400}`.
- `image::tests::packing_is_row_major_msb_first` — a hand-authored 8×2 bit fixture produces its two expected bytes.
- `image::tests::dither_modes_have_stable_golden_snapshots` — the committed 16×16 gradient snapshots match for Bayer and Atkinson and differ from each other.

## Integration / Functional Tests

- `image::tests::process_builds_complete_art_and_palette` — encoded source bytes flow through resize, palette extraction, selected dithering, packed base64, and `Art` metadata.
- `image::tests::pipeline_is_deterministic` — two complete pipeline runs produce identical palette and packed output.

## Smoke Tests

- `cargo fmt --all --check` succeeds.
- `cargo clippy --locked --all-targets --all-features -- -D warnings` succeeds.
- Strict production Clippy from `AGENTS.md` succeeds with panic/unwrap/expect/todo/dbg/print lints denied.
- `cargo test --locked --all-features --all-targets` succeeds with nonzero tests.
- `RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-deps --all-features` succeeds.
- `cargo audit` succeeds in GitHub CI (and locally when the subcommand is installed).

## E2E Tests

- N/A — this issue is a pure image-processing module with no route or user journey.

## Manual / cURL Tests

- N/A — the module performs no network or filesystem I/O. Review the two inline 16×16 base64 golden snapshots and run `cargo test image::tests` to reproduce them.
