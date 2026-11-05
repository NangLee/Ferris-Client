# Ferris drawing worker

An independent Rust graphics worker for prepared Ferris scenes. It draws and encodes videos using community hardware. It cannot open `.osr`, `.osu`, or `.osz` files, and it does not contain Juz's replay parsing, scoring, simulation, or game layout code.

```sh
cargo build --manifest-path crates/ferris-graphics/Cargo.toml --bin ferris-worker --release --locked
```

The Ferris Client downloads a signed runtime package, verifies an assigned scene, extracts it safely, and launches:

```text
ferris-worker --scene <DIRECTORY> --out <MP4> --ffmpeg <FFMPEG_EXECUTABLE>
```

GPU drawing is the default and requires a working integrated or discrete graphics adapter. The client checks the adapter before connecting; the worker fails promptly if GPU initialization later fails. Select CPU mode in the client to use the independent CPU rasterizer (`--cpu-renderer`). Both paths use the same public scene commands. When GPU drawing is active, the worker probes FFmpeg's NVENC encoder with one frame and uses it if available. Otherwise it uses `libx264`. FFmpeg also mixes the prepared media and bounds audio to the video duration.

Community output has a 300 MiB upload limit. The worker budgets 268 MiB for encoded video and audio, limits either encoder's bitrate to fit the scene duration, and checks the finished file. If the required bitrate falls below the configured quality floor, it rejects the scene before drawing frames so Ferris can use the VPS renderer instead.

For Juz's known still-background intro recipe, the worker caches the background and its blur before the timed effects. Other media recipes retain their original FFmpeg graph. The worker logs scene reading, drawing, and FFmpeg pipe time for each render.

`--capabilities` prints the machine readable runtime identity without initializing graphics or starting FFmpeg:

```json
{"runtime_kind":"ferris-scene-worker-v1","scene_format":"ferris-scene-v1","protocol_version":1,"backends":["gpu","cpu"]}
```

The GPU pipeline and shader are fixed parts of this program. Scene packages cannot replace them. The shader is compiled and validated during the Rust build. The software rasterizer applies the same transforms, texture sampling, and blending without any game specific rules.

The worker uses a fixed FFmpeg command with a restricted filter recipe, packaged fonts, local files and frame pipes. It rejects external file reading filters, network protocols, devices, source replays and maps, and unknown drawing commands. It reads one drawing record at a time rather than loading an entire movie into memory.

No account token, application secret, private signing key, or Juz executable is supplied to this process. The client handles authorization, cancellation, heartbeat leases, output upload, and temporary file cleanup. The generic worker itself is reusable software; the valuable boundary is keeping Juz's preparation and game logic on the server.

See [the scene format](../ferris-scene/README.md) for the data contract and the repository's operator guide for deployment and signed releases. The implementation is original and does not copy Miru's rendering source or assets.
