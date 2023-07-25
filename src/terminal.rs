use rustyline_async::{Readline, ReadlineError};
use std::io::Write;

use crate::types::*;

/*
 *  terminal driver
 */
pub async fn terminal(
    our: &Identity,
    version: &str,
    to_event_loop: MessageSender,
    mut print_rx: PrintReceiver,
) -> Result<(), ReadlineError> {
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
            `"  lUL  UU       '^                               888
                     ""                                        888    version {}

            "#,
            version
        )
    );

    let (mut rl, mut stdout) = Readline::new(format!("{} > ", our.name))?;

    loop {
        tokio::select! {
            prints = print_rx.recv() => match prints {
                Some(print) => { writeln!(stdout, "\x1b[38;5;238m{}\x1b[0m", print)?; },
                None => {
                    writeln!(stdout, "terminal: lost print channel, crashing")?;
                    break;
                }
            },
            cmd = rl.readline() => match cmd {
                Ok(line) => {
                    rl.add_history_entry(line.clone());
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
                                    json: Some(serde_json::Value::String(line)),
                                    bytes: None,
                                },
                            }
                        ]
                    ).await;
                }
                Err(ReadlineError::Eof) => {
                    writeln!(stdout, "<EOF>")?;
                    break;
                }
                Err(ReadlineError::Interrupted) => { break; }
                Err(e) => {
                    writeln!(stdout, "terminal error: {e:?}")?;
                    break;
                }
            }
        }
    }
    rl.flush()?;
    Ok(())
}
