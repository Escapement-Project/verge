//! `verge energy watch`: a capture shown live, as a terminal UI, or as a line a read where the
//! output is not a terminal.

use std::collections::VecDeque;
use std::io::IsTerminal;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::Line;
use ratatui::widgets::{Axis, Block, Chart, Dataset, GraphType, Paragraph};
use tracing_indicatif::indicatif_println;
use verge_probe::{Bus, Cancel, CommandError, EnergyCaptureSample, EnergyError, EnergyTotal};

use super::{Pro, range_name, si};

/// A wait is cut into slices this long, so that a key press or Ctrl-C is seen promptly.
const SLICE: Duration = Duration::from_millis(20);

pub struct Options {
    pub rate: u32,
    pub every: Duration,
    pub window: Duration,
    pub limit: Option<Duration>,
}

/// One read of the probe, `at` seconds into the watch.
#[derive(Clone, Copy, Debug)]
struct Reading {
    at: f64,
    newest: Option<Newest>,
    /// The total so far: energy in nJ and the pairs it counts.
    total: Option<(u64, u64)>,
}

/// The newest pair: in units where the firmware calibrates it, as ADC codes where it does not.
#[derive(Clone, Copy, Debug)]
enum Newest {
    Calibrated {
        millivolts: u32,
        nanoamps: i32,
        range: u8,
        sequence: u32,
    },
    Raw {
        voltage_code: u16,
        current_code: u16,
        sequence: u32,
    },
}

pub(crate) struct Model {
    name: String,
    /// The capture shown is one this watch started, and stops when it leaves. A start by another
    /// host makes it that host's.
    started_here: bool,
    /// Pairs a second: the rate this watch started the capture at, or one measured from the
    /// totals for a capture it did not start.
    rate: Option<f64>,
    /// The first total of the capture, from which its rate is measured.
    baseline: Option<(f64, u64)>,
    previous: Option<(u64, u64)>,
    /// The newest pair's sequence at the last read, which counts from 1 at each start.
    sequence: Option<u32>,
    last: Option<Reading>,
    /// Amps of the newest pair at each read.
    current: VecDeque<(f64, f64)>,
    /// Watts over each interval between reads, from the totals.
    power: VecDeque<(f64, f64)>,
    window: f64,
}

impl Model {
    fn new(name: &str, started_at: Option<u32>, window: Duration) -> Self {
        Self {
            name: name.to_owned(),
            started_here: started_at.is_some(),
            rate: started_at.map(f64::from),
            baseline: None,
            previous: None,
            sequence: None,
            last: None,
            current: VecDeque::new(),
            power: VecDeque::new(),
            window: window.as_secs_f64(),
        }
    }

    fn update(&mut self, reading: Reading) {
        if let Some(Newest::Calibrated { nanoamps, .. }) = reading.newest {
            self.current
                .push_back((reading.at, f64::from(nanoamps) * 1e-9));
        }
        let sequence = reading.newest.map(|newest| match newest {
            Newest::Calibrated { sequence, .. } | Newest::Raw { sequence, .. } => sequence,
        });
        // A total that went back, in pairs or in energy, is a start since the last read, and not
        // this watch's: measure it afresh, and leave it running. Without a total the sequence is
        // all there is. With one it is not trusted alone: older firmware could answer the newest
        // pair from a segment back.
        let restarted = match reading.total {
            Some((nanojoules, pairs)) => self
                .previous
                .is_some_and(|(energy, before)| pairs < before || nanojoules < energy),
            None => sequence
                .zip(self.sequence)
                .is_some_and(|(now, before)| now < before),
        };
        if restarted {
            self.started_here = false;
            self.rate = None;
            self.baseline = None;
            self.previous = None;
        }
        if sequence.is_some() {
            self.sequence = sequence;
        }
        if let Some((nanojoules, pairs)) = reading.total {
            let (since, first) = *self.baseline.get_or_insert((reading.at, pairs));
            if !self.started_here && reading.at - since >= 0.5 && pairs > first {
                self.rate = Some((pairs - first) as f64 / (reading.at - since));
            }
            if let (Some(rate), Some((energy_before, pairs_before))) = (self.rate, self.previous)
                && pairs > pairs_before
            {
                let seconds = (pairs - pairs_before) as f64 / rate;
                let joules = (nanojoules - energy_before) as f64 * 1e-9;
                self.power.push_back((reading.at, joules / seconds));
            }
            self.previous = Some((nanojoules, pairs));
        }
        let oldest = reading.at - self.window;
        for series in [&mut self.current, &mut self.power] {
            while series.front().is_some_and(|(at, _)| *at < oldest) {
                series.pop_front();
            }
        }
        self.last = Some(reading);
    }

    fn mean_power(&self) -> Option<f64> {
        let (nanojoules, pairs) = self.last?.total?;
        let seconds = pairs as f64 / self.rate?;
        (seconds > 0.0).then(|| nanojoules as f64 * 1e-9 / seconds)
    }

    fn pair_text(&self) -> String {
        match self.last.and_then(|reading| reading.newest) {
            Some(Newest::Calibrated {
                millivolts,
                nanoamps,
                range,
                sequence,
            }) => format!(
                "{}  {}  ({})  pair {sequence}",
                si(f64::from(millivolts) * 1e-3, "V"),
                si(f64::from(nanoamps) * 1e-9, "A"),
                range_name(range),
            ),
            Some(Newest::Raw {
                voltage_code,
                current_code,
                sequence,
            }) => format!(
                "voltage code {voltage_code}  current code {current_code}  pair {sequence}, not calibrated"
            ),
            None => "no complete pair yet".to_owned(),
        }
    }

    fn total_text(&self) -> String {
        let Some((nanojoules, pairs)) = self.last.and_then(|reading| reading.total) else {
            return "no total yet".to_owned();
        };
        let mut text = format!("{} over {pairs} pairs", si(nanojoules as f64 * 1e-9, "J"));
        if let Some(watts) = self.mean_power() {
            text += &format!(", mean {}", si(watts, "W"));
        }
        if let Some(rate) = self.rate {
            text += &format!(", {rate:.0} pairs a second");
        }
        text
    }

    /// One read as a line, for output that is not a terminal.
    fn line(&self) -> String {
        let at = self.last.map_or(0.0, |reading| reading.at);
        let power = self
            .power
            .back()
            .filter(|(when, _)| *when == at)
            .map_or_else(|| "-".to_owned(), |(_, watts)| si(*watts, "W"));
        format!("{at:7.2} s  {}  interval {power}", self.pair_text())
    }
}

pub(crate) fn render(frame: &mut Frame, model: &Model) {
    let [header, current, power] = Layout::vertical([
        Constraint::Length(5),
        Constraint::Fill(1),
        Constraint::Fill(1),
    ])
    .areas(frame.area());
    let leave = if model.started_here {
        "q leaves and stops the capture this started"
    } else {
        "q leaves; the capture keeps running"
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(model.pair_text()),
            Line::from(model.total_text()),
            Line::from(leave).style(Style::new().fg(Color::DarkGray)),
        ])
        .block(Block::bordered().title(format!(" {} energy ", model.name))),
        header,
    );
    let graph = |title, series, unit, color| Graph {
        title,
        series,
        unit,
        color,
    };
    let current_graph = graph(
        " Current: the newest pair at each read ",
        &model.current,
        "A",
        Color::Cyan,
    );
    let power_graph = graph(
        " Power: each interval's mean, counting every pair ",
        &model.power,
        "W",
        Color::Yellow,
    );
    for (area, graph) in [(current, current_graph), (power, power_graph)] {
        chart(frame, area, &graph, model);
    }
}

struct Graph<'a> {
    title: &'static str,
    series: &'a VecDeque<(f64, f64)>,
    unit: &'static str,
    color: Color,
}

/// One series over the model's window, ending at its last read.
fn chart(frame: &mut Frame, area: Rect, graph: &Graph<'_>, model: &Model) {
    let (title, unit, color, window) = (graph.title, graph.unit, graph.color, model.window);
    let now = model.last.map_or(0.0, |reading| reading.at);
    let points: Vec<(f64, f64)> = graph.series.iter().copied().collect();
    let low = points.iter().map(|(_, y)| *y).fold(0.0, f64::min);
    let high = points.iter().map(|(_, y)| *y).fold(0.0, f64::max);
    let top = if high > low { high * 1.1 } else { low + 1e-9 };
    let x_labels = [
        format!(
            "-{}",
            humantime::format_duration(Duration::from_secs_f64(window))
        ),
        "now".to_owned(),
    ];
    let y_labels = [si(low, unit), si((low + top) / 2.0, unit), si(top, unit)];
    let dataset = Dataset::default()
        .marker(Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::new().fg(color))
        .data(&points);
    frame.render_widget(
        Chart::new(vec![dataset])
            .block(Block::bordered().title(title))
            .x_axis(Axis::default().bounds([now - window, now]).labels(x_labels))
            .y_axis(Axis::default().bounds([low, top]).labels(y_labels)),
        area,
    );
}

/// What a watch leaves: whether a capture still runs, and the total then.
pub struct Ended {
    pub running: bool,
    pub total: Option<EnergyTotal>,
}

/// A fault stops a capture and answers every read until a start clears it, so for a watch it is
/// a capture to start, and for a stop, a capture stopped.
fn fault(error: &CommandError<EnergyError>) -> bool {
    matches!(error, CommandError::Firmware(EnergyError::DmaError))
}

/// Starts a capture unless one runs, shows it until the limit or the user leaves, and stops it
/// again only if it is still the one this started.
pub fn watch<B: Bus>(
    bus: &B,
    name: &str,
    pro: &mut Pro<'_, '_>,
    options: &Options,
    cancel: &Cancel,
) -> Result<Ended> {
    if options.every.is_zero() {
        bail!("--every has to be longer than nothing");
    }
    let totals = pro
        .info()
        .with_context(|| format!("asking {name} what it runs"))?
        .is_some_and(verge_probe::VergeInfo::supports_energy_accumulator);
    let running = match pro.latest_energy_sample() {
        Err(ref error) if fault(error) => {
            // The start below clears what the failed capture kept, so show it first.
            if totals {
                let kept = pro
                    .read_energy_accumulator()
                    .with_context(|| format!("reading {name}'s total"))?;
                indicatif_println!(
                    "{name}: {}. A new capture replaces it.",
                    super::describe_total(name, kept)?
                );
            }
            false
        }
        Ok(EnergyCaptureSample::Stopped) => false,
        Ok(_) => true,
        Err(error) => return Err(error).with_context(|| format!("reading {name}'s capture")),
    };
    if cancel.is_cancelled() {
        bail!("left before the watch began");
    }
    let started_at = if running {
        None
    } else {
        match pro.start_energy_capture(options.rate) {
            Ok(start) => Some(start.actual_rate_pairs_per_second),
            // The reply can be lost after the probe started, so stop what this may have started.
            Err(start) => {
                let start = anyhow!(start).context(format!("starting {name}'s capture"));
                let stop = pro.stop_energy_capture();
                return Err(match stop {
                    Err(stop) if !fault(&stop) => {
                        anyhow!("{start:#}; stopping it again failed too: {stop}")
                    }
                    _ => start,
                });
            }
        }
    };
    let mut model = Model::new(name, started_at, options.window);
    let shown = run(bus, pro, &mut model, options, totals, cancel);
    let stopped = if model.started_here {
        match pro.stop_energy_capture() {
            Err(ref error) if fault(error) => Ok(()),
            stop => stop,
        }
    } else {
        Ok(())
    };
    match (shown, stopped) {
        (Ok(()), Ok(())) => {}
        (Err(shown), Ok(())) => return Err(shown),
        (Ok(()), Err(stop)) => {
            return Err(stop)
                .with_context(|| format!("stopping the capture this started on {name}"));
        }
        (Err(shown), Err(stop)) => {
            bail!("{shown:#}; stopping the capture this started failed too: {stop}")
        }
    }
    let running = match pro.latest_energy_sample() {
        Err(ref error) if fault(error) => false,
        Ok(EnergyCaptureSample::Stopped) => false,
        Ok(_) => true,
        Err(error) => return Err(error).with_context(|| format!("reading {name}'s capture")),
    };
    let total = if totals {
        Some(
            pro.read_energy_accumulator()
                .with_context(|| format!("reading {name}'s total"))?,
        )
    } else {
        None
    };
    Ok(Ended { running, total })
}

fn read(pro: &mut Pro<'_, '_>, at: f64, totals: bool) -> Result<Reading> {
    let newest = match pro
        .latest_energy_sample()
        .context("reading the newest pair")?
    {
        EnergyCaptureSample::Sample(sample) => Some(match sample.calibrated {
            Some(calibrated) => Newest::Calibrated {
                millivolts: calibrated.voltage_millivolts,
                nanoamps: calibrated.current_nanoamps,
                range: calibrated.range_id,
                sequence: sample.raw.sequence,
            },
            None => Newest::Raw {
                voltage_code: sample.raw.voltage_code,
                current_code: sample.raw.current_code,
                sequence: sample.raw.sequence,
            },
        }),
        EnergyCaptureSample::Stopped => {
            bail!("the capture stopped: another host stopped it, or the probe restarted")
        }
        _ => None,
    };
    let total = if totals {
        match pro.read_energy_accumulator().context("reading the total")? {
            EnergyTotal::Total(total) => Some((total.energy_nanojoules, total.integrated_pairs)),
            EnergyTotal::Failed(_) => {
                bail!("the capture failed: the ADC overran or its DMA failed")
            }
            _ => None,
        }
    } else {
        None
    };
    Ok(Reading { at, newest, total })
}

/// Reads and shows the capture until the limit or the user leaves: on the terminal UI where the
/// output is a terminal, as a line a read where it is not.
fn run<B: Bus>(
    bus: &B,
    pro: &mut Pro<'_, '_>,
    model: &mut Model,
    options: &Options,
    totals: bool,
    cancel: &Cancel,
) -> Result<()> {
    let mut screen = if std::io::stdout().is_terminal() {
        // Raw mode comes on first, so a failure after it still owes a restore.
        Some(ratatui::try_init().map_err(|error| {
            ratatui::restore();
            anyhow!(error).context("taking over the terminal")
        })?)
    } else {
        None
    };
    let start = bus.now();
    let deadline = options.limit.map(|limit| start + limit);
    let shown = (|| -> Result<()> {
        loop {
            let at = bus.now().saturating_sub(start).as_secs_f64();
            model.update(read(pro, at, totals)?);
            match &mut screen {
                Some(terminal) => {
                    terminal.draw(|frame| render(frame, model))?;
                }
                None => indicatif_println!("{}", model.line()),
            }
            if deadline.is_some_and(|deadline| bus.now() >= deadline) {
                return Ok(());
            }
            let next = bus.now() + options.every;
            let until = deadline.map_or(next, |deadline| next.min(deadline));
            if wait(bus, until, screen.is_some(), cancel)? {
                return Ok(());
            }
        }
    })();
    if screen.is_some() {
        ratatui::restore();
    }
    shown
}

/// Waits until `until` on the bus's clock, a slice at a time. True when the user asked to leave:
/// with Ctrl-C, which sets `cancel`, or, on the terminal UI, whose raw mode turns Ctrl-C into a
/// key, with Q, Esc or Ctrl-C.
fn wait<B: Bus>(bus: &B, until: Duration, keys: bool, cancel: &Cancel) -> Result<bool> {
    loop {
        if cancel.is_cancelled() || (keys && left_by_key()?) {
            return Ok(true);
        }
        let Some(left) = until.checked_sub(bus.now()).filter(|left| !left.is_zero()) else {
            return Ok(false);
        };
        bus.sleep(SLICE.min(left));
    }
}

fn left_by_key() -> Result<bool> {
    while event::poll(Duration::ZERO)? {
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            let ctrl_c = matches!(key.code, KeyCode::Char('c' | 'C'))
                && key.modifiers.contains(KeyModifiers::CONTROL);
            if ctrl_c || matches!(key.code, KeyCode::Char('q' | 'Q') | KeyCode::Esc) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    fn reading(at: f64, nanoamps: i32, nanojoules: u64, pairs: u64) -> Reading {
        Reading {
            at,
            newest: Some(Newest::Calibrated {
                millivolts: 3300,
                nanoamps,
                range: 1,
                sequence: pairs as u32,
            }),
            total: Some((nanojoules, pairs)),
        }
    }

    #[test]
    fn power_comes_from_the_totals_at_the_rate_this_started() {
        let mut model = Model::new("CJ3E7A91C2", Some(1000), Duration::from_secs(30));
        model.update(reading(0.0, 20_000, 0, 0));
        // 100 pairs at 1000 a second is 0.1 s; 66 µJ over it is 660 µW.
        model.update(reading(0.1, 20_000, 66_000, 100));
        assert_eq!(model.power.back(), Some(&(0.1, 660e-6)));
        assert_eq!(model.mean_power(), Some(660e-6));
    }

    #[test]
    fn a_capture_found_running_has_its_rate_measured() {
        let mut model = Model::new("CJ3E7A91C2", None, Duration::from_secs(30));
        model.update(reading(0.0, 20_000, 1_000_000, 5_000));
        model.update(reading(0.25, 20_000, 1_016_500, 5_250));
        assert!(model.rate.is_none(), "a rate from a quarter second");
        model.update(reading(1.0, 20_000, 1_066_000, 6_000));
        assert_eq!(model.rate, Some(1000.0));
        // 750 pairs at 1000 a second is 0.75 s; 49.5 µJ over it is 66 µW.
        let (_, watts) = *model.power.back().unwrap();
        assert!((watts - 66e-6).abs() < 1e-12, "{watts}");
    }

    #[test]
    fn a_start_by_another_host_is_measured_afresh_and_left_running() {
        let mut model = Model::new("CJ3E7A91C2", Some(1000), Duration::from_secs(30));
        model.update(reading(0.0, 20_000, 500_000, 5_000));
        model.update(reading(0.1, 20_000, 0, 10));
        assert!(!model.started_here, "the capture is the other host's now");
        let points = model.power.len();
        model.update(reading(0.2, 20_000, 6_600, 110));
        assert_eq!(model.power.len(), points, "no rate yet for the new capture");
        // 500 pairs over 0.5 s is 1000 a second; 400 pairs is 0.4 s, and 26.4 µJ over it 66 µW.
        model.update(reading(0.6, 20_000, 33_000, 510));
        assert_eq!(model.rate, Some(1000.0));
        let (at, watts) = *model.power.back().unwrap();
        assert_eq!(at, 0.6);
        assert!((watts - 66e-6).abs() < 1e-12, "{watts}");
    }

    #[test]
    fn energy_that_goes_back_is_another_start_however_many_pairs() {
        let mut model = Model::new("CJ3E7A91C2", Some(1000), Duration::from_secs(30));
        model.update(reading(0.0, 20_000, 500_000, 5_000));
        model.update(reading(0.1, 20_000, 100, 6_000));
        assert!(!model.started_here);
        assert_eq!(model.rate, None);
    }

    #[test]
    fn a_sequence_that_goes_back_is_another_start_without_totals() {
        let mut model = Model::new("CJ3E7A91C2", Some(1000), Duration::from_secs(30));
        let without_total = |at, sequence| Reading {
            total: None,
            ..reading(at, 20_000, 0, sequence)
        };
        model.update(without_total(0.0, 5_000));
        model.update(without_total(0.1, 5_100));
        assert!(model.started_here);
        model.update(without_total(0.2, 40));
        assert!(!model.started_here);
    }

    #[test]
    fn a_sequence_that_steps_back_while_the_total_grows_is_the_same_capture() {
        let mut model = Model::new("CJ3E7A91C2", Some(1000), Duration::from_secs(30));
        model.update(reading(0.0, 20_000, 500_000, 5_000));
        let back = Reading {
            newest: Some(Newest::Calibrated {
                millivolts: 3300,
                nanoamps: 20_000,
                range: 1,
                sequence: 4_993,
            }),
            ..reading(0.1, 20_000, 506_600, 5_100)
        };
        model.update(back);
        assert!(
            model.started_here,
            "a segment-short newest pair is not a start"
        );
        assert_eq!(model.rate, Some(1000.0));
    }

    #[test]
    fn the_graphs_keep_the_window_and_no_more() {
        let mut model = Model::new("CJ3E7A91C2", Some(1000), Duration::from_secs(1));
        for n in 0..30u32 {
            model.update(reading(
                f64::from(n) * 0.1,
                20_000,
                6_600 * u64::from(n),
                100 * u64::from(n),
            ));
        }
        // Reads 0.1 s apart over a 1 s window: ten or eleven, as float steps land.
        assert!(
            (10..=11).contains(&model.current.len()),
            "{:?}",
            model.current
        );
        assert!((10..=11).contains(&model.power.len()), "{:?}", model.power);
        assert!(model.current.front().unwrap().0 >= 2.9 - 1.0 - 1e-9);
    }

    #[test]
    fn an_uncalibrated_pair_shows_its_codes_and_draws_nothing() {
        let mut model = Model::new("CJ3E7A91C2", None, Duration::from_secs(30));
        model.update(Reading {
            at: 0.0,
            newest: Some(Newest::Raw {
                voltage_code: 1234,
                current_code: 567,
                sequence: 9,
            }),
            total: None,
        });
        assert_eq!(
            model.pair_text(),
            "voltage code 1234  current code 567  pair 9, not calibrated"
        );
        assert!(model.current.is_empty());
    }

    #[test]
    fn the_screen_shows_the_pair_the_total_and_both_graphs() {
        let mut model = Model::new("CJ3E7A91C2", Some(1000), Duration::from_secs(30));
        for n in 0..20u32 {
            let nanoamps = if n % 10 == 0 { 5_000_000 } else { 20_000 };
            model.update(reading(
                f64::from(n) * 0.1,
                nanoamps,
                171_000 * u64::from(n),
                100 * u64::from(n),
            ));
        }
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| render(frame, &model)).unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content()
            .chunks(100)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("CJ3E7A91C2 energy"), "{screen}");
        assert!(
            screen.contains("3.30 V  20.0 µA  (350 mA, high gain)  pair 1900"),
            "{screen}"
        );
        assert!(
            screen.contains("3.25 mJ over 1900 pairs, mean 1.71 mW, 1000 pairs a second"),
            "{screen}"
        );
        assert!(
            screen.contains("q leaves and stops the capture this started"),
            "{screen}"
        );
        assert!(
            screen.contains("Current: the newest pair at each read"),
            "{screen}"
        );
        assert!(screen.contains("Power: each interval's mean"), "{screen}");
        assert!(screen.contains("-30s"), "{screen}");
        // Both graphs plot their points, in Braille dots.
        let (current, power) = screen.split_at(screen.find("Power:").unwrap());
        let dots = |text: &str| {
            text.chars()
                .filter(|c| ('\u{2801}'..='\u{28FF}').contains(c))
                .count()
        };
        assert!(dots(current) >= 3, "{screen}");
        assert!(dots(power) >= 3, "{screen}");
    }
}
