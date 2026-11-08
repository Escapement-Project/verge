# Fonts

These fonts are the Escapement Project's type, as its design tokens set it. They are copies of
files the project's site repository, `escapement-project.github.io`, generates in
`design/out/fonts/`. Its `design/build.py` takes each family's variable font from google/fonts, at
the commit its `design/tokens.toml` pins, and fixes its axes at the values below, because egui has
no variable axes. Each file is named as the font's own style table names that instance. Unlike the
site's web fonts, these are not subset.

| File | Role in verge-gui | Axes |
| --- | --- | --- |
| `Saira-Expanded-Medium.ttf` | display: the slab's word, the window's name, dialog titles | width 125, weight 500 |
| `HankenGrotesk-Regular.ttf` | body text | weight 400 |
| `HankenGrotesk-Medium.ttf` | labels and buttons | weight 500 |
| `HankenGrotesk-SemiBold.ttf` | instructions | weight 600 |
| `MartianMono-SemiExpanded-Medium.ttf` | figures that change in place: readings, timings, progress, counts | width 112.5, weight 500 |
| `MartianMono-Medium.ttf` | code: serials, hashes, paths, the log | width 100, weight 500 |

All three families are under the SIL Open Font License 1.1, with no Reserved Font Name. Each
family's licence text sits beside its files: `saira-OFL.txt`, `hanken-grotesk-OFL.txt` and
`martian-mono-OFL.txt`. The window's Licences button shows them too.

`src/tokens.rs` and the icons in `icons/` are copies from the same build. To check every copy
against it, run this from the site repository:

```sh
uv run design/build.py --check ../verge/gui/src/tokens.rs ../verge/gui/fonts/*.ttf \
    ../verge/gui/fonts/*-OFL.txt ../verge/gui/icons/*.png
```

It compares each copy with the output of the same name, byte for byte. To take a new build, copy
the files over and run the check.
