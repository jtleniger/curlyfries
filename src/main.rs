//! Binary entry point for `curlyfries`.

fn main() -> std::process::ExitCode {
    curlyfries::main_with(
        std::env::args_os(),
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
}
