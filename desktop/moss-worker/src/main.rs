use std::process::ExitCode;

fn main() -> ExitCode {
    match echowall_moss_worker::run_once() {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => {
            eprintln!("echowall_moss_worker_error:{code}");
            ExitCode::FAILURE
        }
    }
}
