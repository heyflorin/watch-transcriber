use std::process::ExitCode;

fn main() -> ExitCode {
    match echowall_summary_worker::run_once() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("echowall_summary_worker_error:{}", error.code());
            ExitCode::FAILURE
        }
    }
}
