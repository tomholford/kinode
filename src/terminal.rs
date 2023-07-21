use std::io::Write;
use rustyline_async::{Readline, ReadlineError};

use crate::types::*;

/*
 *  terminal driver
 */
pub async fn terminal(our: &Identity, version: &str, to_event_loop: MessageSender, mut print_rx: PrintReceiver)
    -> Result<(), ReadlineError> {

    // print initial splash screen
    println!("\x1b[38;5;128m{}\x1b[0m", format!(r#"

                ,,   UU
            s#  lUL  UU       !p
           !UU  lUL  UU       !UUlb
       #U  !UU  lUL  UU       !UUUUU#
       UU  !UU  lUL  UU       !UUUUUUUb
       UU  !UU  %"     ;-     !UUUUUUUU#
   $   UU  !UU         @UU#p  !UUUUUUUUU#
  ]U   UU  !#          @UUUUS !UUUUUUUUUUb
  @U   UU  !           @UUUUUUlUUUUUUUUUUU                         888
  UU   UU  !           @UUUUUUUUUUUUUUUUUU                         888
  @U   UU  !           @UUUUUU!UUUUUUUUUUU                         888
  'U   UU  !#          @UUUU# !UUUUUUUUUU~       888  888  .d88888 88888b.   8888b.  888d888
   \   UU  !UU         @UU#^  !UUUUUUUUU#        888  888 d88" 888 888 "88b     "88b 888P"
       UU  !UU  @Np  ,,"      !UUUUUUUU#         888  888 888  888 888  888 .d888888 888
       UU  !UU  lUL  UU       !UUUUUUU^          Y88b 888 Y88b 888 888 d88P 888  888 888
       "U  !UU  lUL  UU       !UUUUUf             "Y88888  "Y88888 88888P"  "Y888888 888
           !UU  lUL  UU       !UUl^                            888
            `"  lUL  UU       '^                               888
                     ""                                        888    version {}

            "#, version));


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
                    match parse_command(our.name.as_str(), &line).unwrap_or(Command::Invalid) {
                        Command::StartOfMessageStack(messages) => {
                            to_event_loop.send(messages).await.unwrap();
                            // writeln!(stdout, "{}", line)?;
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
    let (head, tail) = line.split_once(" ").unwrap_or((line, ""));
    match head {
        "!message" => {
            let (target_server, tail) = tail.split_once(" ")?;
            let (target_app, payload) = tail.split_once(" ")?;
            let val = serde_json::from_str::<serde_json::Value>(payload).ok()?;
            Some(Command::StartOfMessageStack(vec![Message {
                message_type: MessageType::Request(false),
                wire: Wire {
                    source_ship: our_name.to_string(),
                    source_app: "terminal".to_string(),
                    target_ship: target_server.to_string(),
                    target_app: target_app.to_string(),
                },
                payload: Payload {
                    json: Some(val),
                    bytes: None,
                },
            }]))
        }
        "!quit" => Some(Command::Quit),
        "!exit" => Some(Command::Quit),
        _ => None,
    }
}
