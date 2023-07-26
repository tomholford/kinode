use crossterm::{
    cursor,
    event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers},
    execute,
    style::Print,
    terminal::{self, disable_raw_mode, enable_raw_mode, ClearType},
};
use futures::{future::FutureExt, StreamExt};
use std::collections::VecDeque;
use std::io::{stdout, Write};

use crate::types::*;

struct CommandHistory {
    pub lines: VecDeque<String>,
    pub max_size: usize,
    pub index: usize,
}

impl CommandHistory {
    fn new(max_size: usize) -> Self {
        Self {
            lines: VecDeque::with_capacity(max_size),
            max_size,
            index: 0,
        }
    }

    fn add(&mut self, line: String) {
        self.lines.push_front(line);
        self.index = 0;
        if self.lines.len() > self.max_size {
            self.lines.pop_back();
        }
    }

    fn get_prev(&mut self) -> String {
        if self.lines.len() == 0 || self.index == self.lines.len() {
            return "".into();
        }
        let line = self.lines[self.index].clone();
        if self.index < self.lines.len() {
            self.index += 1;
        }
        line
    }

    fn get_next(&mut self) -> String {
        if self.lines.len() == 0 || self.index == 0 {
            return "".into();
        }
        self.index -= 1;
        self.lines[self.index].clone()
    }
}

/*
 *  terminal driver
 */
pub async fn terminal(
    our: &Identity,
    version: &str,
    event_loop: MessageSender,
    debug_event_loop: DebugSender,
    print_tx: PrintSender,
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
    let prompt_len: u16 = (our.name.len() + 3).try_into().unwrap();
    let (_win_cols, win_rows) = terminal::size().unwrap();
    let mut cursor_col = prompt_len;
    let mut in_step_through: bool = false;
    // TODO add more verbosity levels as needed?
    // defaulting to TRUE for now, as we are BUIDLING
    let mut verbose_mode: bool = true;

    // TODO make adjustable max history length
    let mut command_history = CommandHistory::new(1000);

    loop {
        let event = reader.next().fuse();

        tokio::select! {
            prints = print_rx.recv() => match prints {
                Some(printout) => {
                    let mut stdout = stdout.lock();
                    if match printout.verbosity {
                        0 => false,
                        1 => !verbose_mode,
                        _ => true
                    } {
                        continue;
                    }
                    execute!(
                        stdout,
                        cursor::MoveTo(0, win_rows - 1),
                        terminal::Clear(ClearType::CurrentLine),
                        Print(format!("\x1b[38;5;238m{}\x1b[0m\r\n", printout.content)),
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
                        // turn off the node
                        Event::Key(KeyEvent {
                            code: KeyCode::Char('c'),
                            modifiers: KeyModifiers::CONTROL,
                            ..
                        }) => {
                            break;
                        },
                        // CTRL+V: toggle verbose mode
                        Event::Key(KeyEvent {
                            code: KeyCode::Char('v'),
                            modifiers: KeyModifiers::CONTROL,
                            ..
                        }) => {
                            verbose_mode = !verbose_mode;
                        },
                        // CTRL+D: toggle debug mode -- makes system-level event loop step-through
                        // CTRL+S: step through system-level event loop
                        Event::Key(KeyEvent {
                            code: KeyCode::Char('d'),
                            modifiers: KeyModifiers::CONTROL,
                            ..
                        }) => {
                            let _ = print_tx.send(
                                Printout {
                                    verbosity: 0,
                                    content: match in_step_through {
                                        true => "debug mode off".into(),
                                        false => "debug mode on: use CTRL+S to step through events".into(),
                                    }
                                }
                            ).await;
                            let _ = debug_event_loop.send(DebugCommand::Toggle).await;
                            in_step_through = !in_step_through;
                        },
                        Event::Key(KeyEvent {
                            code: KeyCode::Char('s'),
                            modifiers: KeyModifiers::CONTROL,
                            ..
                        }) => {
                            let _ = debug_event_loop.send(DebugCommand::Step).await;
                        },
                        //
                        //  handle keypress events
                        //
                        Event::Key(k) => {
                            match k.code {
                                KeyCode::Char(c) => {
                                    current_line.insert(cursor_col as usize, c);
                                    cursor_col += 1;
                                    execute!(
                                        stdout,
                                        cursor::MoveTo(0, win_rows),
                                        Print(&current_line),
                                        cursor::MoveTo(cursor_col, win_rows),
                                    )?;
                                },
                                KeyCode::Backspace => {
                                    if cursor::position().unwrap().0 == prompt_len {
                                        continue;
                                    }
                                    cursor_col -= 1;
                                    current_line.remove(cursor_col as usize);
                                    execute!(
                                        stdout,
                                        cursor::MoveTo(0, win_rows),
                                        terminal::Clear(ClearType::CurrentLine),
                                        Print(&current_line),
                                    )?;
                                },
                                KeyCode::Left => {
                                    if cursor::position().unwrap().0 == prompt_len {
                                        continue;
                                    }
                                    execute!(
                                        stdout,
                                        cursor::MoveLeft(1),
                                    )?;
                                    cursor_col -= 1;
                                },
                                KeyCode::Right => {
                                    if cursor::position().unwrap().0 as usize == current_line.len() {
                                        continue;
                                    }
                                    execute!(
                                        stdout,
                                        cursor::MoveRight(1),
                                    )?;
                                    cursor_col += 1;
                                },
                                KeyCode::Up => {
                                    // go up one command in history
                                    current_line = format!("{} > {}", our.name, command_history.get_prev());
                                    cursor_col = current_line.len() as u16;
                                    execute!(
                                        stdout,
                                        cursor::MoveTo(0, win_rows),
                                        terminal::Clear(ClearType::CurrentLine),
                                        Print(&current_line),
                                    )?;
                                },
                                KeyCode::Down => {
                                    // go down one command in history
                                    current_line = format!("{} > {}", our.name, command_history.get_next());
                                    cursor_col = current_line.len() as u16;
                                    execute!(
                                        stdout,
                                        cursor::MoveTo(0, win_rows),
                                        terminal::Clear(ClearType::CurrentLine),
                                        Print(&current_line),
                                    )?;
                                },
                                KeyCode::Enter => {
                                    let command = current_line[prompt_len as usize..].to_string();
                                    current_line = format!("{} > ", our.name);
                                    execute!(
                                        stdout,
                                        Print("\r\n"),
                                        Print(&current_line),
                                    )?;
                                    command_history.add(command.clone());
                                    cursor_col = prompt_len;
                                    let _err = event_loop.send(
                                        WrappedMessage {
                                            id: rand::random(),
                                            rsvp: None,
                                            message: Message {
                                                message_type: MessageType::Request(false),
                                                wire: Wire {
                                                    source_ship: our.name.clone(),
                                                    source_app: "terminal".into(),
                                                    target_ship: our.name.clone(),
                                                    target_app: "terminal".into(),
                                                },
                                                payload: Payload {
                                                    json: None,
                                                    bytes: bincode::serialize(&command).ok(),
                                                },
                                            }
                                        }
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
