use std::io::Write;
use rustyline_async::{Readline, ReadlineError};

use crate::types::*;

/*
 *  terminal driver
 */
pub async fn terminal(our_name: &str, card_tx: CardSender, mut print_rx: PrintReceiver)
    -> Result<(), ReadlineError> {

    let (mut rl, mut stdout) = Readline::new("> ".into())?;

    loop {
        tokio::select! {
            prints = print_rx.recv() => match prints {
                Some(print) => { writeln!(stdout, "{}", print)?; },
                None => { break; }
            },
            cmd = rl.readline() => match cmd {
                Ok(line) => {
                    rl.add_history_entry(line.clone());
                    match parse_command(our_name, &line).unwrap_or(Command::Invalid) {
                        Command::Card(card) => {
                            card_tx.send(card).await.unwrap();
                            writeln!(stdout, "{}", line)?;
                        },
                        Command::Quit => {
                            break;
                        },
                        Command::Invalid => {
                            writeln!(stdout, "invalid command: {}", line)?;
                        }
                    }
                }
                Err(ReadlineError::Eof) => {
                    writeln!(stdout, "<EOF>")?;
                    break;
                }
                Err(ReadlineError::Interrupted) => { break; }
                Err(e) => {
                    writeln!(stdout, "Error: {e:?}")?;
                    break;
                }
            }
        }
    }
    rl.flush()?;
    Ok(())
}

fn parse_command(our_name: &str, line: &str) -> Option<Command> {
    let (head, tail) = line.split_once(" ")?;
    match head {
        "!card" => {
            let (target, payload) = tail.split_once(" ")?;
            let val = serde_json::from_str::<serde_json::Value>(payload).ok()?;
            Some(Command::Card(Card {
                source: our_name.to_string(),
                target: target.to_string(),
                payload: val,
            }))
        }
        "!quit" => Some(Command::Quit),
        "!exit" => Some(Command::Quit),
        _ => None,
    }
}
