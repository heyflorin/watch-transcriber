use std::{env, process::ExitCode};

fn run() -> Result<String, &'static str> {
    let mut args = env::args_os().skip(1);
    let manifest = args.next().ok_or("manifest_required")?;
    if args.next().is_some() {
        return Err("unexpected_argument");
    }
    let report = echowall_local_quality_eval::evaluate_speaker_coverage(manifest)
        .map_err(echowall_local_quality_eval::EvalError::code)?;
    serde_json::to_string(&report).map_err(|_| "report_failed")
}

fn main() -> ExitCode {
    match run() {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(code) => {
            eprintln!("speaker_coverage_error:{code}");
            ExitCode::FAILURE
        }
    }
}
