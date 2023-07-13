use std::io::Write;
use rustyline_async::{Readline, ReadlineError};

use crate::types::*;

/*
 *  terminal driver
 */
pub async fn terminal(our_name: &str, message_tx: MessageSender, mut print_rx: PrintReceiver)
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
                        Command::Message(message) => {
                            message_tx.send(message).await.unwrap();
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
    if line == "\n" { return None }
    let (head, tail) = line.split_once(" ")?;
    match head {
        "!message" => {
            let (target_server, tail) = tail.split_once(" ")?;
            let (target_app, payload) = tail.split_once(" ")?;
            let val = serde_json::from_str::<serde_json::Value>(payload).ok()?;
            Some(Command::Message(Message {
                note: Note::Pass, // TODO I believe this is correct
                wire: Wire {
                    source_ship: our_name.to_string(),
                    source_app: "terminal".to_string(),
                    target_ship: target_server.to_string(),
                    target_app: target_app.to_string(),
                },
                payload: Payload::Json(val),
            }))
        }
        "!quit" => Some(Command::Quit),
        "!exit" => Some(Command::Quit),
        _ => None,
    }
}
