use crossterm::{
    cursor,
    event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers},
    execute,
    style::Print,
    terminal::{self, disable_raw_mode, enable_raw_mode, ClearType},
};
use futures::{future::FutureExt, StreamExt};
use std::io::{stdout, Write};

use crate::types::*;

/*
 *  terminal driver
 */
pub async fn terminal(
    our: &Identity,
    version: &str,
    to_event_loop: MessageSender,
    mut print_rx: PrintReceiver,
) -> std::io::Result<()> {
    execute!(stdout(), terminal::Clear(ClearType::All))?;

    // print initial splash screen
    println!(
        "\x1b[38;5;128m{}\x1b[0m",
        format!(
            r#"

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
            `"  lUL  UU       '^                               888    {}
                     ""                                        888    version {}

            "#,
            our.name, version
        )
    );

    enable_raw_mode()?;
    let stdout = stdout();
    let mut reader = EventStream::new();
    let mut current_line = format!("{} > ", our.name);
    let prompt_len = our.name.len() + 3;
    let (win_cols, win_rows) = terminal::size().unwrap();

    // TODO set a max history length
    let mut command_history: Vec<String> = vec![];
    let mut command_history_index: usize = 1;

    loop {
        let event = reader.next().fuse();

        tokio::select! {
            prints = print_rx.recv() => match prints {
                Some(print) => {
                    let mut stdout = stdout.lock();
                    execute!(
                        stdout,
                        cursor::MoveTo(0, win_rows - 1),
                        terminal::Clear(ClearType::CurrentLine),
                        Print(format!("\x1b[38;5;238m{}\x1b[0m\r\n", print)),
                        cursor::MoveTo(0, win_rows),
                        Print(&current_line),
                    )?;
                },
                None => {
                    write!(stdout.lock(), "terminal: lost print channel, crashing")?;
                    break;
                }
            },
            maybe_event = event => match maybe_event {

                Some(Ok(event)) => {
                    let mut stdout = stdout.lock();
                    match event {
                        Event::Key(KeyEvent {
                            code: KeyCode::Char('c'),
                            modifiers: KeyModifiers::CONTROL,
                            ..
                        }) => {
                            break;
                        },
                        Event::Key(k) => {
                            match k.code {
                                KeyCode::Char(c) => {
                                    execute!(
                                        stdout,
                                        Print(c),
                                    )?;
                                    current_line.push(c);
                                },
                                KeyCode::Backspace => {
                                    if cursor::position().unwrap().0 as usize == prompt_len {
                                        continue;
                                    }
                                    execute!(
                                        stdout,
                                        cursor::MoveLeft(1),
                                        Print(" "),
                                        cursor::MoveLeft(1),
                                    )?;
                                    current_line.pop();
                                },
                                KeyCode::Up => {
                                    // go up one command in history,
                                    // saving current line to history
                                    if command_history_index == 0 {
                                        continue;
                                    }
                                    let has = command_history.get(command_history_index - 1);
                                    match has {
                                        None => continue,
                                        Some(got) => {
                                            let got = got.clone();
                                            if command_history_index > command_history.len() {
                                                command_history.push(current_line[prompt_len..].to_string());
                                            }
                                            execute!(
                                                stdout,
                                                cursor::MoveTo(0, win_rows),
                                                terminal::Clear(ClearType::CurrentLine),
                                                Print(format!("{} > {}", our.name, got)),
                                            )?;
                                            command_history_index -= 1;
                                        }
                                    }
                                },
                                KeyCode::Down => {
                                    // go down one command in history
                                    let has = command_history.get(command_history_index + 1);
                                    match has {
                                        None => continue,
                                        Some(got) => {
                                            execute!(
                                                stdout,
                                                cursor::MoveTo(0, win_rows),
                                                terminal::Clear(ClearType::CurrentLine),
                                                Print(format!("{} > {}", our.name, got)),
                                            )?;
                                            command_history_index += 1;
                                        }
                                    }
                                },
                                KeyCode::Enter => {
                                    let command = current_line[prompt_len..].to_string();
                                    command_history.push(command.clone());
                                    command_history_index = command_history.len();
                                    current_line = format!("{} > ", our.name);
                                    execute!(
                                        stdout,
                                        Print("\r\n"),
                                        Print(&current_line),
                                    )?;
                                    let _err = to_event_loop.send(
                                        vec![
                                            Message {
                                                message_type: MessageType::Request(false),
                                                wire: Wire {
                                                    source_ship: our.name.clone(),
                                                    source_app: "terminal".into(),
                                                    target_ship: our.name.clone(),
                                                    target_app: "terminal".into(),
                                                },
                                                payload: Payload {
                                                    json: Some(serde_json::Value::String(command)),
                                                    bytes: None,
                                                },
                                            }
                                        ]
                                    ).await;
                                },
                                _ => {},
                            }
                        },
                        _ => {},
                    }
                }
                Some(Err(e)) => println!("Error: {:?}\r", e),
                None => break,
            }
        }
    }
    disable_raw_mode()
}
