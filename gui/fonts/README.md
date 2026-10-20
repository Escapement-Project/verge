# Fonts

Each face is unmodified, taken from its project's own release on 2026-10-20. All are under the
SIL Open Font License 1.1, whose text sits beside them.

| File | Role | Release | SHA-256 |
| --- | --- | --- | --- |
| `IBMPlexSansCondensed-Medium.ttf` | titles, instructions, actions; the display role for now | [IBM/plex `@ibm/plex-sans-condensed@2.0.0`](https://github.com/IBM/plex/releases/tag/%40ibm%2Fplex-sans-condensed%402.0.0), `fonts/complete/ttf/` | `0c6b79ef4a7de76ffc6bf2091980e5f81a1a87ac3967566bc98aae0baa518f54` |
| `IBMPlexSans-Light.ttf` | body text | [IBM/plex `@ibm/plex-sans@1.1.0`](https://github.com/IBM/plex/releases/tag/%40ibm%2Fplex-sans%401.1.0), `fonts/complete/ttf/` | `2218b5f3f1fc9d3a793343f45c3be5ee7eae9584a7a52391bb8cf2d37622b3e0` |
| `IBMPlexMono-Regular.ttf` | serials, hashes, paths, the log | [IBM/plex `@ibm/plex-mono@2.5.0`](https://github.com/IBM/plex/releases/tag/%40ibm%2Fplex-mono%402.5.0), `fonts/complete/ttf/` | `7c6fbddca4b700be918f5f6183d9bd4464fa427fe435f0b480d77fe2bb8c5a43` |
| `KodeMono-Bold.ttf` | labels and versions | [isaozler/kode-mono `v2.003`](https://github.com/isaozler/kode-mono/releases/tag/v2.003), `fonts/ttf/` | `2863cdaaa2647edc1bb6851e4927122df9f9ac74de57589c898bbd7808fd7952` |

`IBMPlex-LICENSE.txt` is the licence in all three Plex releases, which are identical. It reserves
the name "Plex": a Plex file that is subset or otherwise changed must be renamed first.
`KodeMono-OFL.txt` comes from Kode Mono's release.

The display role has no face of its own yet. It uses Plex Sans Condensed Medium through one named
text style (`style::Role::Display`), so a chosen face replaces it in `style.rs` alone.
