use std::env;
use std::ffi::OsString;
use std::process::ExitCode;

use echowall_local_quality_eval::AcceptancePolicy;

fn parse_arguments(
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<(OsString, AcceptancePolicy), &'static str> {
    let manifest = arguments.next().ok_or("manifest_required")?;
    let Some(flag) = arguments.next() else {
        return Ok((manifest, AcceptancePolicy::LegacyV1));
    };
    if flag != "--policy" {
        return Err("unexpected_argument");
    }
    let policy = match arguments
        .next()
        .as_deref()
        .and_then(std::ffi::OsStr::to_str)
    {
        Some("legacy-v1") => AcceptancePolicy::LegacyV1,
        Some("miaoji-relative-v2") => AcceptancePolicy::MiaojiRelativeV2,
        _ => return Err("unsupported_policy"),
    };
    if arguments.next().is_some() {
        return Err("unexpected_argument");
    }
    Ok((manifest, policy))
}

fn main() -> ExitCode {
    let (manifest, policy) = match parse_arguments(env::args_os().skip(1)) {
        Ok(arguments) => arguments,
        Err(code) => {
            eprintln!("local_quality_eval_error:{code}");
            return ExitCode::FAILURE;
        }
    };
    match echowall_local_quality_eval::evaluate_manifest_with_policy(&manifest, policy) {
        Ok(report) => serde_json::to_string(&report).map_or_else(
            |_| {
                eprintln!("local_quality_eval_error:report_failed");
                ExitCode::FAILURE
            },
            |json| {
                println!("{json}");
                ExitCode::SUCCESS
            },
        ),
        Err(error) => {
            eprintln!("local_quality_eval_error:{}", error.code());
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_requires_explicit_selection_and_rejects_unknown_values() {
        let parse = |args: &[&str]| parse_arguments(args.iter().map(OsString::from));
        assert_eq!(
            parse(&["matrix.json"]).unwrap().1,
            AcceptancePolicy::LegacyV1
        );
        assert_eq!(
            parse(&["matrix.json", "--policy", "legacy-v1"]).unwrap().1,
            AcceptancePolicy::LegacyV1
        );
        assert_eq!(
            parse(&["matrix.json", "--policy", "miaoji-relative-v2"])
                .unwrap()
                .1,
            AcceptancePolicy::MiaojiRelativeV2
        );
        assert_eq!(parse(&[]).unwrap_err(), "manifest_required");
        assert_eq!(
            parse(&["matrix.json", "--policy"]).unwrap_err(),
            "unsupported_policy"
        );
        assert_eq!(
            parse(&["matrix.json", "--policy", "private-unknown-value"]).unwrap_err(),
            "unsupported_policy"
        );
        assert_eq!(
            parse(&["matrix.json", "--policy", "legacy-v1", "extra"]).unwrap_err(),
            "unexpected_argument"
        );
    }
}
