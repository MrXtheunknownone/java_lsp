use java_lsp::handshake::ExitAction;
use java_lsp::jsonrpc::{self, Incoming, OutgoingNotification};
use java_lsp::server::Server;
use java_lsp::transport;
use std::io::{self, BufReader, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    run_server()
}

fn run_server() -> ExitCode {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("failed to start background task runtime");
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    let mut server = Server::new(runtime.handle().clone());

    loop {
        let body = match transport::read_message(&mut reader) {
            Ok(Some(body)) => body,
            Ok(None) => return ExitCode::FAILURE,
            Err(err) => {
                eprintln!("failed to read message: {err}");
                return ExitCode::FAILURE;
            }
        };

        let incoming: Incoming = match jsonrpc::parse_incoming(&body) {
            Ok(incoming) => incoming,
            Err(response) => {
                if let Err(err) = write_json(&mut writer, &response) {
                    eprintln!("failed to write message: {err}");
                    return ExitCode::FAILURE;
                }
                continue;
            }
        };

        match incoming {
            Incoming::Request(request) => {
                let response = server.handle_request(&request);
                if let Err(err) = write_json(&mut writer, &response) {
                    eprintln!("failed to write message: {err}");
                    return ExitCode::FAILURE;
                }
            }
            Incoming::Notification(notification) => {
                let (exit_action, outgoing) = server.handle_notification(&notification);
                if let Err(err) = write_notifications(&mut writer, &outgoing) {
                    eprintln!("failed to write message: {err}");
                    return ExitCode::FAILURE;
                }
                match exit_action {
                    ExitAction::Continue => {}
                    ExitAction::Exit(0) => return ExitCode::SUCCESS,
                    ExitAction::Exit(_) => return ExitCode::FAILURE,
                }
            }
        }
    }
}

fn write_json(writer: &mut impl Write, value: &impl serde::Serialize) -> io::Result<()> {
    let body = serde_json::to_vec(value).expect("LSP message value always serializes");
    transport::write_message(writer, &body)
}

fn write_notifications(
    writer: &mut impl Write,
    notifications: &[OutgoingNotification],
) -> io::Result<()> {
    for notification in notifications {
        write_json(writer, notification)?;
    }
    Ok(())
}
