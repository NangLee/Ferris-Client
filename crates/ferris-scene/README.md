# Ferris scene format, version 1

This crate contains generic drawing data and streaming I/O. It contains no replay reader, map reader, judgment system, timing simulation, or game assets.

A scene package is a ZIP with this layout:

```text
manifest.json
commands.fscene.gz
media/
    0.png
    2.ogg
    3.wav
    font-0.ttf
```

`manifest.json` serializes `Manifest`. It defines the output size, frame rate, exact frame count, and `MediaRecipe`. The recipe keeps media inputs in their original order. Exactly one input is `{ "type": "frames" }`; other inputs are `{ "type": "file", "path": "media/2.ogg", "mode": "once" }`. File modes are `once`, `still`, and `repeat`.

The recipe references video and optional audio filter output labels, such as `[vfinal]` and `[aout]`. Font references in the filter graph use `fontfile='@font/0'`, pointing to the first item in `fonts`. Paths are relative to the package. The worker accepts a restricted set of media filters, never arbitrary executable commands or shader programs.

## Drawing stream

The gzip payload begins with the eight bytes `FERRIS01`. Each record consists of a one byte type followed by a four byte little endian payload length and then its payload:

| Type | Payload |
| --- | --- |
| 1 | Texture: little endian `u32` ID, width, height, followed by tightly packed RGBA8 pixels |
| 2 | UTF-8 JSON serialization of `Frame` |
| 3 | Texture removal: little endian `u32` ID |
| 255 | End of stream, empty payload |

The texture ID is an opaque number. A texture record creates or replaces it. Texture removal releases it. A frame may only use textures that are currently present.

Frames have a clear color, final output alpha, and an ordered list of drawing commands. Commands draw transformed sprite batches or replace pixels outside a circle. Colors are normalized RGBA floats. Sprite coordinates use output pixels. Rotation is radians around the sprite's local pixel origin. UV coordinates permit cropping and flipping. Texture sampling is nearest with edges clamped. Alpha, additive, and copy blend modes are available.

The final frame alpha is multiplied by `output_alpha` after all drawing, leaving RGB unchanged. A circle mask compares integer pixel coordinates against its center and radius and copies its color outside the circle.

## Bounds

- A record is at most 64 MiB.
- The entire uncompressed drawing stream is at most 8 GiB and two million records.
- Live textures occupy at most 512 MiB in the worker.
- Frame commands and sprite counts are bounded separately.
- Movies have at most 240 frames per second, one hour duration, 8192 pixels on either axis, and 16,777,216 output pixels.
- The client separately limits ZIP transfers and extraction, verifies the server's signed job permit and package hash, and rejects source replays, source maps, executables, and links.

These bounds cause unsupported or oversized jobs to fall back to the server. They do not silently remove effects from a render.

The server vendors this crate under Juz's `vendor/ferris-scene`. Keep the canonical client crate and that server copy identical when changing the protocol. Incompatible changes need a new format and worker capability identifier.
