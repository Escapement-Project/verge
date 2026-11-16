# verge

A command-line tool and a desktop app to install, update and control Verge, the open CMSIS-DAP
firmware for NXP's MCU-Link and MCU-Link Pro (mculink-verge). They update NXP's firmware too, and
drive the DIO pins of both probes. On a Pro they also switch the target supply and record energy
captures. Both are built on verge-probe.

[Verge's page](https://escapement-project.github.io/verge/) covers using them. Its docs live here
and in mculink-verge:

- [Getting started](docs/getting-started.md): installing verge, and the first update.
- [Updating and recovery](docs/updating.md): how each firmware updates, the jumpers, ISP mode,
  updating many probes, and releases.
- [The desktop app](docs/desktop-app.md): verge-gui.
- [Energy capture](docs/energy.md): measuring a target's current and energy on the Pro.
- [The verge command](docs/command.md): every command, and the doc for each.

This README is for building verge and making its releases.

## Building

From the repository's root:

```
cargo build --release
```

The CLI is `target/release/verge`; the GUI is `target/release/verge-gui`.

On Linux, building needs the libudev headers (`libudev-dev` on Debian and Ubuntu,
`systemd-devel` on Fedora). The HID library that talks to the bootloader uses them.

### Releases of verge

`.github/workflows/release.yml` builds verge and verge-gui when a tag `v<version>` is pushed, the
version matching the workspace version in `Cargo.toml`. It makes one archive per platform, with a
`.sha256` file and a minisign signature made with the firmware release key; [Getting
started](docs/getting-started.md#install-verge) lists them and says how to check one.

Signing uses a `release` environment with a required reviewer. Configure it as described in
mculink-verge's README under "Releases".

## Contributing

[CONTRIBUTING.md](CONTRIBUTING.md) has how the tools are built and how to test them.

## Not done yet

- Variant releases. A release carries one image, so a firmware build with a variant suffix, such
  as the `+armwire` reference build, cannot be published beside the plain build.
- The J-Link firmware that LinkServer ships for the MCU-Link. The tool does not recognise a unit
  running it.
- Stock V2.x, which speaks HID only. Its USB product string has not been recorded from a real
  unit. The simulator's is a guess, and so is the version the tool reads from it.

## Licence

Apache-2.0, in `LICENSE`. The fonts in `gui/fonts/` are under the SIL Open Font License, with
their licence texts beside them. The GUI's Licences button displays the texts too.
