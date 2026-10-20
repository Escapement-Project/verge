//! Detection and path choice against the simulated bench, whose probes answer from the recorded
//! identities.

use serde_json::Value;
use verge_core::detect::{Guess, State};
use verge_core::sim::{SimBus, SimFirmware};
use verge_core::version::StockVersion;
use verge_core::{Firmware, Path, detect, offer};

/// The recorded `DAP_Info` block for `label`, read here rather than through the simulator.
fn recorded(label: &str) -> Value {
    let text = include_str!("../src/sim/recorded.json");
    let all: Value = serde_json::from_str(text).unwrap();
    all["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["label"] == label)
        .unwrap()
        .clone()
}

#[test]
fn the_bench_reads_back_as_recorded() {
    let bus = SimBus::scenario("bench").unwrap();
    let units = detect(&bus).unwrap();
    assert_eq!(units.len(), 2);

    for (unit, label) in units.iter().zip(["gcc", "stock"]) {
        let record = recorded(label);
        let info = &record["dap_info"];
        let State::Probe { identity, firmware } = &unit.state else {
            panic!("{label}: {:?}", unit.state);
        };
        assert_eq!(
            identity.vendor.as_deref(),
            info["vendor"].as_str(),
            "{label}"
        );
        assert_eq!(
            identity.product.as_deref(),
            info["product"].as_str(),
            "{label}"
        );
        assert_eq!(
            identity.serial.as_deref(),
            info["serial"].as_str(),
            "{label}"
        );
        assert_eq!(
            identity.protocol.as_deref(),
            info["protocol version"].as_str(),
            "{label}"
        );
        assert_eq!(
            identity.firmware_version.as_deref(),
            info["firmware version"].as_str(),
            "{label}"
        );
        assert_eq!(
            identity.packet_count.map(u64::from),
            info["max packet count"].as_u64(),
            "{label}"
        );
        assert_eq!(
            identity.packet_size.map(u64::from),
            info["max packet size"].as_u64(),
            "{label}"
        );
        match (firmware, &record["ours"]) {
            (Firmware::Ours(ours), Value::Object(recorded)) => {
                assert_eq!(u64::from(ours.format), recorded["format"].as_u64().unwrap());
                assert_eq!(u64::from(ours.flags), recorded["flags"].as_u64().unwrap());
                assert_eq!(ours.build, recorded["build"].as_str().unwrap());
            }
            (Firmware::Stock { version }, Value::Null) => {
                assert_eq!(*version, StockVersion::parse("3.172"));
            }
            other => panic!("{label}: {other:?}"),
        }
    }
    assert_eq!(units[0].state.to_string(), "ours, build 14d05d4");
    assert_eq!(units[1].state.to_string(), "stock V3.172");
}

/// A stock version other than the recorded one must not read back as the recorded strings.
#[test]
fn another_stock_version_reads_back_as_itself() {
    let bus = SimBus::default();
    bus.add_stock("3.108");
    let units = detect(&bus).unwrap();
    let State::Probe { identity, firmware } = &units[0].state else {
        panic!("{:?}", units[0].state);
    };
    assert_eq!(identity.firmware_version.as_deref(), Some("3.108"));
    assert_ne!(
        identity.product,
        recorded("stock")["dap_info"]["product"]
            .as_str()
            .map(str::to_owned)
    );
    assert_eq!(
        *firmware,
        Firmware::Stock {
            version: StockVersion::parse("3.108")
        }
    );
}

#[test]
fn replies_left_by_an_earlier_host_are_drained_first() {
    let bus = SimBus::default();
    let unit = bus.add_ours("14d05d4");
    bus.leave_stale_replies(unit, vec![vec![0x05, 0x00, 0x01], vec![0x06, 0x00]]);
    let units = detect(&bus).unwrap();
    let State::Probe { identity, .. } = &units[0].state else {
        panic!("{:?}", units[0].state);
    };
    assert_eq!(identity.serial.as_deref(), Some("CJ3E7A91C2"));
}

#[test]
fn a_probe_held_by_another_program_is_named_and_guessed() {
    let bus = SimBus::scenario("busy").unwrap();
    let units = detect(&bus).unwrap();
    assert_eq!(
        units[0].state,
        State::Unreadable {
            reason: "another program has it open".into(),
            guess: Guess::Ours,
        }
    );
    let offer = offer(&units[0], &units);
    assert_eq!(offer.paths, [Path::Jumper]);
}

#[test]
fn stock_v2_speaks_hid_only_and_gets_the_jumper() {
    let bus = SimBus::scenario("stock-hid").unwrap();
    let units = detect(&bus).unwrap();
    let State::Unreadable { guess, .. } = &units[0].state else {
        panic!("{:?}", units[0].state);
    };
    assert_eq!(*guess, Guess::Stock(StockVersion::parse("2.245")));
    assert_eq!(offer(&units[0], &units).paths, [Path::Jumper]);
}

#[test]
fn every_scenario_offers_what_its_state_allows() {
    let cases: &[(&str, &[&[Path]])] = &[
        (
            "bench",
            &[
                &[Path::OursJumperFree, Path::Jumper],
                &[Path::StockJumperFree, Path::Jumper],
            ],
        ),
        ("ours", &[&[Path::OursJumperFree, Path::Jumper]]),
        ("stock", &[&[Path::StockJumperFree, Path::Jumper]]),
        ("stock-old", &[&[Path::Jumper]]),
        ("isp", &[&[Path::FlashNow]]),
        ("isp-jumper", &[&[Path::FlashNow]]),
        ("two-isp", &[&[], &[]]),
        ("mini", &[&[]]),
        ("isp-mini", &[&[], &[]]),
    ];
    for (name, expected) in cases {
        let bus = SimBus::scenario(name).unwrap();
        let units = detect(&bus).unwrap();
        let got: Vec<Vec<Path>> = units.iter().map(|u| offer(u, &units).paths).collect();
        let expected: Vec<Vec<Path>> = expected.iter().map(|p| p.to_vec()).collect();
        assert_eq!(got, expected, "{name}");
    }
}

#[test]
fn ours_is_told_from_stock_by_the_identify_reply_alone() {
    // A unit whose USB strings look like stock but which answers the identify command is ours.
    let bus = SimBus::default();
    bus.add_running(SimFirmware::Ours {
        build: "v1.0.0+armwire".into(),
    });
    let units = detect(&bus).unwrap();
    let build = units[0].build().unwrap();
    assert_eq!(build.variant.as_deref(), Some("armwire"));
}
