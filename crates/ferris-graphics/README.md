# Ferris drawing worker

An independent Rust graphics worker for prepared Ferris scenes. It draws and encodes videos using community hardware. It cannot open `.osr`, `.osu`, or `.osz` files, and it does not contain Juz's replay parsing, scoring, simulation, or game layout code.

```sh
cargo build --manifest-path crates/ferris-graphics/Cargo.toml --bin ferris-worker --release --locked
```

The Ferris Client downloads a signed runtime package, verifies an assigned scene, extracts it safely, and launches:

```text
ferris-worker --scene <DIRECTORY> --out <MP4> --ffmpeg <FFMPEG_EXECUTABLE>
```

GPU drawing is the default. `--cpu-renderer` selects the independent CPU rasterizer. GPU initialization failure also falls back to CPU drawing with a log message. Both paths use the same public scene commands. FFmpeg encodes H.264 using `libx264` and mixes the prepared media. GPU drawing and hardware video encoding are separate concerns; this initial worker uses software H.264 encoding for consistent compatibility.

`--capabilities` prints the machine readable runtime identity without initializing graphics or starting FFmpeg:

```json
{"runtime_kind":"ferris-scene-worker-v1","scene_format":"ferris-scene-v1","protocol_version":1,"backends":["gpu","cpu"]}
```

The GPU pipeline and shader are fixed parts of this program. Scene packages cannot replace them. The shader is compiled and validated during the Rust build. The software rasterizer applies the same transforms, texture sampling, and blending without any game specific rules.

The worker uses a fixed FFmpeg command with a restricted filter recipe, packaged fonts, local files and frame pipes. It rejects external file reading filters, network protocols, devices, source replays and maps, and unknown drawing commands. It reads one drawing record at a time rather than loading an entire movie into memory.

No account token, application secret, private signing key, or Juz executable is supplied to this process. The client handles authorization, cancellation, heartbeat leases, output upload, and temporary file cleanup. The generic worker itself is reusable software; the valuable boundary is keeping Juz's preparation and game logic on the server.

See [the scene format](../ferris-scene/README.md) for the data contract and the repository's operator guide for deployment and signed releases. The implementation is original and does not copy Miru's rendering source or assets.
