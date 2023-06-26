use std::io::Write;
use rustyline_async::{Readline, ReadlineError};

use crate::types::*;

/*
 *  terminal driver
 */
pub async fn terminal(our_name: &str, card_tx: CardSender, mut print_rx: PrintReceiver)
    -> core::result::Result<(), ReadlineError> {

    let (mut rl, mut stdout) = Readline::new("> ".into()).unwrap();

    loop {
        tokio::select! {
            prints = print_rx.recv() => match prints {
                Some(print) => { writeln!(stdout, "{}", print)?; },
                None => { break; }
            },
            cmd = rl.readline() => match cmd {
                Ok(line) => {
                    rl.add_history_entry(line.clone());
                    match parse_command(our_name, &line) {
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

fn parse_command(our_name: &str, line: &str) -> Command {
    let mut parts = line.split_whitespace();
    match parts.next() {
        Some("!card") => {
            Command::Card(Card {
                source: our_name.to_string(),
                target: parts.next().unwrap().to_string(),
                payload: serde_json::json!({"message": parts.collect::<Vec<&str>>().join(" ")}),
            })
        }
        Some("!quit") => Command::Quit,
        Some("!exit") => Command::Quit,
        _ => Command::Invalid,
    }
}
