use std::process::ExitCode;

fn main() -> ExitCode {
    match beam_port::run_stdio() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}
