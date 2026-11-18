//! Questions at a terminal, and the ways a run ends that are not the tool failing.

use std::fmt;
use std::io::{self, IsTerminal};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, bail};
use dialoguer::console::Term;

/// The exit status of a flash that went in, on a unit that does not run it yet: it came back in
/// ISP mode, and the message says what to do.
pub const EXIT_NOT_RUNNING: u8 = 3;
/// The exit status after Ctrl-C.
pub const EXIT_STOPPED: u8 = 130;

/// How a run ends without the tool failing: a no, Ctrl-C at a question, or a flash the unit does
/// not run yet. `main` prints `message`, where there is one, without the ERROR prefix, and exits
/// with `code`.
#[derive(Debug)]
pub struct Ended {
    pub message: Option<String>,
    pub code: u8,
}

impl Ended {
    pub fn new(message: impl Into<String>, code: u8) -> Ended {
        Ended {
            message: Some(message.into()),
            code,
        }
    }

    /// Ends with `code`, everything said already.
    pub fn quietly(code: u8) -> Ended {
        Ended {
            message: None,
            code,
        }
    }
}

impl fmt::Display for Ended {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message.as_deref().unwrap_or("ended"))
    }
}

impl std::error::Error for Ended {}

/// Set while a question waits for its answer. Ctrl-C then ends the question, which says so
/// itself, rather than an operation.
static ASKING: AtomicBool = AtomicBool::new(false);

pub fn asking() -> bool {
    ASKING.load(Ordering::Relaxed)
}

/// A line on stderr, above any progress bars.
pub fn say(text: &str) {
    tracing_indicatif::suspend_tracing_indicatif(|| eprintln!("{text}"));
}

/// Whether a question can be asked here: dialoguer reads the answer from stdin and draws the
/// question on stderr.
pub fn interactive() -> bool {
    io::stdin().is_terminal() && io::stderr().is_terminal()
}

/// What " [y/N] " and an answer of "yes" add to a prompt's last row.
const PROMPT_TAIL: usize = 8;

/// Asks `question` after the sentences in `lead`, unless `yes` answers it. Off a terminal it
/// refuses in one line that says to pass `--yes`.
pub fn confirm(lead: &[String], question: &str, yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        bail!(
            "{}{question} Pass --yes to go ahead without being asked",
            joined(lead)
        );
    }
    ask(lead, question)
}

/// Asks at a terminal, for a question `--yes` does not answer.
pub fn confirm_at_terminal(lead: &[String], question: &str) -> Result<()> {
    if !io::stdin().is_terminal() {
        bail!(
            "{}{question} Only an answer at a terminal says yes to this; --yes does not",
            joined(lead)
        );
    }
    ask(lead, question)
}

fn joined(lead: &[String]) -> String {
    lead.iter().map(|sentence| format!("{sentence} ")).collect()
}

/// A no is [`Ended`] with "Nothing was flashed." and status 1.
fn ask(lead: &[String], question: &str) -> Result<()> {
    let width = usize::from(Term::stderr().size().1).max(2 * PROMPT_TAIL);
    for sentence in lead {
        for line in textwrap::wrap(sentence, width - 1) {
            say(&line);
        }
    }
    // dialoguer redraws only the prompt's last row once it is answered, so the rows before it are
    // printed here, once.
    let mut rows = textwrap::wrap(question, width - PROMPT_TAIL);
    let last = rows.pop().unwrap_or_default().into_owned();
    for row in rows {
        say(&row);
    }
    let go = answered(true, || {
        dialoguer::Confirm::new()
            .with_prompt(last)
            .default(false)
            .interact()
    })?;
    if !go {
        return Err(Ended::new("Nothing was flashed.", 1).into());
    }
    Ok(())
}

/// Picks one of `items` at a terminal, after `prompt`.
pub fn choose(prompt: &str, items: &[String]) -> Result<usize> {
    answered(false, || {
        dialoguer::Select::new()
            .with_prompt(prompt)
            .items(items)
            .default(0)
            .interact()
    })
}

/// A question's answer. Ctrl-C at the question is [`Ended`] with status 130: dialoguer reads keys
/// raw, sees Ctrl-C as a key and hides the cursor without showing it again. `on_its_line` says the
/// cursor is still on the question's line then, as a yes or no question leaves it.
fn answered<T>(on_its_line: bool, start: impl FnOnce() -> dialoguer::Result<T>) -> Result<T> {
    ASKING.store(true, Ordering::Relaxed);
    match start() {
        Err(dialoguer::Error::IO(error)) if error.kind() == io::ErrorKind::Interrupted => {
            // The key's own signal is raised on this thread, which blocks it. ASKING stays set,
            // so one sent from elsewhere ends nothing else either.
            let _ = Term::stderr().show_cursor();
            let stopped = "Stopped at the question. Nothing was changed.";
            Err(Ended::new(
                if on_its_line {
                    format!("\n{stopped}")
                } else {
                    stopped.to_owned()
                },
                EXIT_STOPPED,
            )
            .into())
        }
        result => {
            ASKING.store(false, Ordering::Relaxed);
            Ok(result?)
        }
    }
}
