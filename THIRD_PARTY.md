# Third-party software and reference provenance


The application uses Rust crates listed in Cargo.toml and pinned in Cargo.lock. These crates retain their own licenses, including eframe/egui, Tokio, reqwest, Rustls, keyring, ed25519-dalek, sysinfo, tray-icon, zip, tar and xz2. Distributions must retain applicable notices. Generate a complete dependency report for the exact shipped lockfile with a license-reporting tool such as cargo-about before a public release.

The version 0.2 community runtime is the independently written generic `ferris-worker` in `crates/ferris-graphics`, using `crates/ferris-scene` for scene data. Its graphics dependencies include wgpu, bytemuck and pollster, with the exact versions recorded in that crate's Cargo.lock. It does not link Juz, include Juz assets, or process original osu! replays and mapsets. Juz remains a separate server component with its own licensing obligations.

Prepared scenes can contain third-party music, images, fonts, skin textures, and other media. Sending them to a worker does not change their ownership or grant redistribution rights. Retain the rights and notices needed for the media used by the service.

FFmpeg builds containing libx264 generally use the GPL. Preserve the build's license and corresponding-source information when redistributing it. Using an upstream HTTPS download URL in a signed manifest does not relicense the package.

Icon was ai generated and will soon be repalced by actual human art

References:

- https://github.com/emilk/egui
- https://ffmpeg.org/legal.html
